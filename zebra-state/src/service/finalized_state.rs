//! The primary implementation of the `zebra_state::Service` built upon rocksdb.
//!
//! Zebra's database is implemented in 4 layers:
//! - [`FinalizedState`]: queues, validates, and commits blocks, using...
//! - [`ZebraDb`]: reads and writes [`zebra_chain`] types to the state database, using...
//! - [`DiskDb`]: reads and writes generic types to any column family in the database, using...
//! - [`disk_format`]: converts types to raw database bytes.
//!
//! These layers allow us to split [`zebra_chain`] types for efficient database storage.
//! They reduce the risk of data corruption bugs, runtime inconsistencies, and panics.
//!
//! # Correctness
//!
//! [`crate::constants::state_database_format_version_in_code()`] must be incremented
//! each time the database format (column, serialization, etc) changes.

use std::{
    io::{stderr, stdout, Write},
    sync::Arc,
    time::Instant,
};

use zebra_chain::{
    amount::{DeferredPoolBalanceChange, NonNegative},
    block,
    history_tree::HistoryTree,
    parallel::tree::{NoteCommitmentTreeError, NoteCommitmentTrees},
    parameters::{
        subsidy::{block_subsidy_with_parent_pools, SubsidyError},
        Network,
    },
    primitives::zcash_history::BlockCommitmentTreeRoots,
    value_balance::ValueBalance,
};
use zebra_db::{
    chain::BLOCK_INFO,
    transparent::{BALANCE_BY_TRANSPARENT_ADDR, TX_LOC_BY_SPENT_OUT_LOC},
};

use crate::{
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    error::CommitCheckpointVerifiedError,
    request::{FinalizableBlock, FinalizedBlock, Treestate},
    service::{check, ChainTipBlock},
    CheckpointVerifiedBlock, Config, StateInitError, ValidateContextError,
};

/// Records the time since the `Option<Instant>` in `$timer` as the block write task phase
/// `$phase`, then resets `$timer` to now, so consecutive phases can share one timer.
///
/// Does nothing if `$timer` is `None`. `$phase` must be a literal, so the metric key is static and
/// recording a sample doesn't allocate.
macro_rules! record_phase {
    ($timer:expr, $phase:literal) => {
        if let Some(start) = $timer.as_mut() {
            let now = std::time::Instant::now();
            metrics::histogram!(
                "state.block_writer.phase.duration_seconds",
                "phase" => $phase
            )
            .record(now.duration_since(*start).as_secs_f64());
            *start = now;
        }
    };
}
pub(crate) use record_phase;

pub mod column_family;

mod disk_db;
mod disk_format;
mod zebra_db;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use column_family::{TypedColumnFamily, WriteTypedBatch};
#[allow(unused_imports)]
pub use disk_db::{DiskDb, DiskWriteBatch, ReadDisk, WriteDisk};
#[allow(unused_imports)]
pub use disk_format::{
    FromDisk, IntoDisk, OutputLocation, RawBytes, TransactionIndex, TransactionLocation,
    MAX_ON_DISK_HEIGHT,
};
pub use zebra_db::ZebraDb;

#[cfg(any(test, feature = "proptest-impl"))]
pub use disk_format::KV;

pub use disk_format::upgrade::restorable_db_versions;

/// Minimum Sapling, Orchard and Ironwood note commitments in a checkpoint block for its note
/// commitment tree update to run alongside its history commitment check.
const OVERLAP_COMMITMENT_THRESHOLD: usize = 32;

/// The column families supported by the running `zebra-state` database code.
///
/// Existing column families that aren't listed here are preserved when the database is opened.
pub const STATE_COLUMN_FAMILIES_IN_CODE: &[&str] = &[
    // Blocks
    "hash_by_height",
    "height_by_hash",
    "block_header_by_height",
    // Transactions
    "tx_by_loc",
    "hash_by_tx_loc",
    "tx_loc_by_hash",
    // Transparent
    BALANCE_BY_TRANSPARENT_ADDR,
    "tx_loc_by_transparent_addr_loc",
    "utxo_by_out_loc",
    "utxo_loc_by_transparent_addr_loc",
    TX_LOC_BY_SPENT_OUT_LOC,
    // Sprout
    "sprout_nullifiers",
    "sprout_anchors",
    "sprout_note_commitment_tree",
    // Sapling
    "sapling_nullifiers",
    "sapling_anchors",
    "sapling_note_commitment_tree",
    "sapling_note_commitment_subtree",
    // Orchard
    "orchard_nullifiers",
    "orchard_anchors",
    "orchard_note_commitment_tree",
    "orchard_note_commitment_subtree",
    // Ironwood (NU6.3). Always registered so the database format is stable across build
    // flags; these stay empty until NU6.3 transactions appear.
    "ironwood_nullifiers",
    "ironwood_anchors",
    "ironwood_note_commitment_tree",
    "ironwood_note_commitment_subtree",
    // Chain
    "history_tree",
    "tip_chain_value_pool",
    BLOCK_INFO,
];

/// The finalized tip's data, carried by the block write task from one checkpoint commit to the
/// next, so each commit doesn't read and decode it from the database again.
///
/// Only [`FinalizedState::commit_checkpoint_verified`] creates or uses this cache. It is only
/// valid for the next commit by the same block write task, because:
/// - nothing else writes blocks while the block write task is committing checkpoint-verified
///   blocks,
/// - it isn't returned after a failed commit, or created while a database format upgrade (which
///   can rewrite the history tree) is running,
/// - it lives in memory only, so a restart reads the database again.
///
/// Debug builds check that it matches the database before every use.
#[derive(Clone, Debug)]
pub(crate) struct CheckpointTipCache {
    /// The height and hash of the finalized tip.
    pub(crate) tip: (block::Height, block::Hash),

    /// The history tree at the finalized tip.
    pub(crate) history_tree: Arc<HistoryTree>,

    /// The chain value pools at the finalized tip.
    pub(crate) value_pool: ValueBalance<NonNegative>,
}

impl CheckpointTipCache {
    /// Panics if this cache doesn't match the finalized tip data in `db`.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn assert_matches_db(&self, db: &ZebraDb) {
        use disk_format::chain::HistoryTreeParts;

        let history_tree_bytes = |tree: &HistoryTree| {
            tree.as_ref()
                .map(|tree| HistoryTreeParts::from(tree).as_bytes())
        };

        assert_eq!(Some(self.tip), db.tip(), "cached finalized tip is stale");
        assert_eq!(
            history_tree_bytes(&self.history_tree),
            history_tree_bytes(&db.history_tree()),
            "cached finalized history tree is stale",
        );
        assert_eq!(
            self.value_pool,
            db.finalized_value_pool(),
            "cached finalized value pool is stale",
        );
    }
}

/// The finalized part of the chain state, stored in the db.
///
/// `rocksdb` allows concurrent writes through a shared reference,
/// so clones of the finalized state represent the same database instance.
/// When the final clone is dropped, the database is closed.
///
/// This is different from `NonFinalizedState::clone()`,
/// which returns an independent copy of the chains.
#[derive(Clone, Debug)]
pub struct FinalizedState {
    // Configuration
    //
    // This configuration cannot be modified after the database is initialized,
    // because some clones would have different values.
    //
    /// The configured stop height.
    ///
    /// Commit blocks to the finalized state up to this height, then exit Zebra.
    debug_stop_at_height: Option<block::Height>,

    // Owned State
    //
    // Everything contained in this state must be shared by all clones, or read-only.
    //
    /// The underlying database.
    ///
    /// `rocksdb` allows reads and writes via a shared reference,
    /// so this database object can be freely cloned.
    /// The last instance that is dropped will close the underlying database.
    pub db: ZebraDb,
}

impl FinalizedState {
    /// Returns an on-disk database instance for `config` and `network`.
    /// If there is no existing database, creates a new database on disk.
    pub fn new(config: &Config, network: &Network) -> Result<Self, StateInitError> {
        Self::new_with_debug(config, network, false, false)
    }

    /// Returns an on-disk database instance with the supplied production and debug settings.
    /// If there is no existing database, creates a new database on disk.
    ///
    /// This method is intended for use in tests.
    #[allow(clippy::unwrap_in_result)]
    pub(crate) fn new_with_debug(
        config: &Config,
        network: &Network,
        debug_skip_format_upgrades: bool,
        read_only: bool,
    ) -> Result<Self, StateInitError> {
        let db = ZebraDb::new(
            config,
            STATE_DATABASE_KIND,
            &state_database_format_version_in_code(),
            network,
            debug_skip_format_upgrades,
            STATE_COLUMN_FAMILIES_IN_CODE
                .iter()
                .map(ToString::to_string),
            read_only,
        )?;

        let new_state = Self {
            debug_stop_at_height: config.debug_stop_at_height.map(block::Height),
            db,
        };

        // TODO: move debug_stop_at_height into a task in the start command (#3442)
        if let Some(tip_height) = new_state.db.finalized_tip_height() {
            if new_state.is_at_stop_height(tip_height) {
                let debug_stop_at_height = new_state
                    .debug_stop_at_height
                    .expect("true from `is_at_stop_height` implies `debug_stop_at_height` is Some");
                let tip_hash = new_state.db.finalized_tip_hash();

                if tip_height > debug_stop_at_height {
                    tracing::error!(
                        ?debug_stop_at_height,
                        ?tip_height,
                        ?tip_hash,
                        "previous state height is greater than the stop height",
                    );
                }

                tracing::info!(
                    ?debug_stop_at_height,
                    ?tip_height,
                    ?tip_hash,
                    "state is already at the configured height"
                );

                // RocksDB can do a cleanup when column families are opened.
                // So we want to drop it before we exit.
                std::mem::drop(new_state);

                // Drops tracing log output that's hasn't already been written to stdout
                // since this exits before calling drop on the WorkerGuard for the logger thread.
                // This is okay for now because this is test-only code
                //
                // TODO: Call ZebradApp.shutdown or drop its Tracing component before calling exit_process to flush logs to stdout
                Self::exit_process();
            }
        }

        Ok(new_state)
    }

    /// Returns the configured network for this database.
    pub fn network(&self) -> Network {
        self.db.network()
    }

    /// Commit a checkpoint-verified block to the state.
    ///
    /// Returns the committed chain-tip data and updated note commitment trees.
    ///
    /// It's the caller's responsibility to ensure that blocks are committed in
    /// order.
    /// Runs on the blocking state writer; the caller publishes the tip before notifying completion.
    pub fn commit_finalized(
        &mut self,
        checkpoint_verified: CheckpointVerifiedBlock,
        prev_note_commitment_trees: Option<NoteCommitmentTrees>,
    ) -> Result<(ChainTipBlock, NoteCommitmentTrees), CommitCheckpointVerifiedError> {
        self.commit_checkpoint_verified(checkpoint_verified, prev_note_commitment_trees, None, None)
            .map(|(tip_block, note_commitment_trees, _tip_cache)| {
                (tip_block, note_commitment_trees)
            })
    }

    /// Commit a checkpoint-verified block to the state, like [`Self::commit_finalized`].
    ///
    /// `tip_cache` must be the cache returned by the previous commit, or `None` to read the
    /// finalized tip's data from the database. Returns the committed chain-tip data, the updated
    /// note commitment trees, and, if it can be used for the next commit, the new tip cache.
    ///
    /// `precomputed_trees`, if present, must be the result of updating the parent's note commitment
    /// trees with this block, and is used instead of updating them here.
    pub(crate) fn commit_checkpoint_verified(
        &mut self,
        checkpoint_verified: CheckpointVerifiedBlock,
        prev_note_commitment_trees: Option<NoteCommitmentTrees>,
        tip_cache: Option<CheckpointTipCache>,
        precomputed_trees: Option<Result<NoteCommitmentTrees, NoteCommitmentTreeError>>,
    ) -> Result<
        (
            ChainTipBlock,
            NoteCommitmentTrees,
            Option<CheckpointTipCache>,
        ),
        CommitCheckpointVerifiedError,
    > {
        let tip_block = ChainTipBlock {
            hash: checkpoint_verified.hash,
            height: checkpoint_verified.height,
            time: checkpoint_verified.block.header.time,
            transactions: checkpoint_verified.block.transactions.clone(),
            transaction_hashes: checkpoint_verified.transaction_hashes.clone(),
            previous_block_hash: checkpoint_verified.block.header.previous_block_hash,
        };
        let result = self.commit_finalized_direct_with_cache(
            checkpoint_verified.into(),
            prev_note_commitment_trees,
            tip_cache,
            precomputed_trees,
            "commit checkpoint-verified request",
        );

        if result.is_ok() {
            metrics::counter!("state.checkpoint.finalized.block.count").increment(1);
            metrics::gauge!("state.checkpoint.finalized.block.height")
                .set(tip_block.height.0 as f64);

            // This height gauge is updated for both fully verified and checkpoint blocks.
            // These updates can't conflict, because the state makes sure that blocks
            // are committed in order.
            metrics::gauge!("zcash.chain.verified.block.height").set(tip_block.height.0 as f64);
            metrics::counter!("zcash.chain.verified.block.total").increment(1);
        } else {
            metrics::counter!("state.checkpoint.error.block.count").increment(1);
            metrics::gauge!("state.checkpoint.error.block.height").set(tip_block.height.0 as f64);
        };

        result.map(|(_hash, note_commitment_trees, tip_cache)| {
            (tip_block, note_commitment_trees, tip_cache)
        })
    }

    /// Immediately commit a `finalized` block to the finalized state.
    ///
    /// This can be called either by the non-finalized state (when finalizing
    /// a block) or by the checkpoint verifier.
    ///
    /// Use `source` as the source of the block in log messages.
    ///
    /// # Errors
    ///
    /// - Propagates any errors from writing to the DB
    /// - Propagates any errors from updating history and note commitment trees
    /// - If `hashFinalSaplingRoot` / `hashLightClientRoot` / `hashBlockCommitments`
    ///   does not match the expected value
    pub fn commit_finalized_direct(
        &mut self,
        finalizable_block: FinalizableBlock,
        prev_note_commitment_trees: Option<NoteCommitmentTrees>,
        source: &str,
    ) -> Result<(block::Hash, NoteCommitmentTrees), CommitCheckpointVerifiedError> {
        self.commit_finalized_direct_with_cache(
            finalizable_block,
            prev_note_commitment_trees,
            None,
            None,
            source,
        )
        .map(|(hash, note_commitment_trees, _tip_cache)| (hash, note_commitment_trees))
    }

    /// Immediately commit a `finalized` block to the finalized state, like
    /// [`Self::commit_finalized_direct`].
    ///
    /// For checkpoint-verified blocks, uses `tip_cache` instead of reading the finalized tip's
    /// data from the database, and returns the new tip cache. See [`CheckpointTipCache`] for when
    /// the cache is used. Blocks finalized by the non-finalized state never use or return it.
    ///
    /// For checkpoint-verified blocks, uses `precomputed_trees` instead of updating the note
    /// commitment trees, if present. It must be the parent's trees updated with this block.
    #[allow(clippy::unwrap_in_result)]
    fn commit_finalized_direct_with_cache(
        &mut self,
        finalizable_block: FinalizableBlock,
        prev_note_commitment_trees: Option<NoteCommitmentTrees>,
        tip_cache: Option<CheckpointTipCache>,
        precomputed_trees: Option<Result<NoteCommitmentTrees, NoteCommitmentTreeError>>,
        source: &str,
    ) -> Result<
        (block::Hash, NoteCommitmentTrees, Option<CheckpointTipCache>),
        CommitCheckpointVerifiedError,
    > {
        let is_checkpoint = matches!(finalizable_block, FinalizableBlock::Checkpoint { .. });

        // Format upgrades can rewrite the history tree in another thread, so only use or create
        // a cache once they have finished. This flag never goes back to `false`.
        let use_tip_cache = is_checkpoint && self.db.finished_format_upgrades();
        let tip_cache = tip_cache.filter(|_| use_tip_cache);

        #[cfg(debug_assertions)]
        if let Some(tip_cache) = &tip_cache {
            tip_cache.assert_matches_db(&self.db);
        }

        let (parent_tip, parent_history_tree, cached_value_pool) = match tip_cache {
            Some(CheckpointTipCache {
                tip,
                history_tree,
                value_pool,
            }) => (Some(tip), Some(history_tree), Some(value_pool)),
            None => (None, None, None),
        };

        // Only checkpoint commits are timed, so the `checkpoint_*` phases don't mix in blocks
        // finalized by the non-finalized state.
        let mut phase_timer = is_checkpoint.then(Instant::now);

        // The parent's value pools, which the block write also needs.
        let parent_value_pool;
        let (height, hash, finalized, prev_note_commitment_trees) = match finalizable_block {
            FinalizableBlock::Checkpoint {
                checkpoint_verified,
            } => {
                // Checkpoint-verified blocks don't have an associated treestate, so we retrieve the
                // treestate of the finalized tip from the database and update it for the block
                // being committed, assuming the retrieved treestate is the parent block's
                // treestate. Later on, this function proves this assumption by asserting that the
                // finalized tip is the parent block of the block being committed.

                let block = checkpoint_verified.block.clone();
                let mut history_tree =
                    parent_history_tree.unwrap_or_else(|| self.db.history_tree());
                record_phase!(phase_timer, "checkpoint_history_tree_load");

                let prev_note_commitment_trees = prev_note_commitment_trees
                    .unwrap_or_else(|| self.db.note_commitment_trees_for_tip());

                // The commitment check uses only the parent history tree, so it can overlap the
                // note commitment tree update. NU5 authorizing data is not bound by the block hash;
                // checkpoint blocks need this check even after their headers have been verified.
                let mut note_commitment_trees = prev_note_commitment_trees.clone();
                let network = self.network();
                let precomputed_auth_data_root = checkpoint_verified.precomputed_auth_data_root();
                let check_commitment = || {
                    check::block_commitment_is_valid_for_chain_history(
                        block.clone(),
                        &network,
                        &history_tree,
                        precomputed_auth_data_root,
                    )
                };
                // Waking the Rayon pool for every small block costs more CPU than the overlap
                // saves, so only overlap blocks with a substantial tree update.
                let shielded_commitments = block.sapling_note_commitments().count()
                    + block.orchard_note_commitments().count()
                    + block.ironwood_note_commitments().count();
                let (tree_result, commitment_result) = if let Some(precomputed) = precomputed_trees
                {
                    #[cfg(debug_assertions)]
                    if let Ok(precomputed) = &precomputed {
                        let mut expected = prev_note_commitment_trees.clone();
                        expected
                            .update_trees_parallel(&block)
                            .expect("the precomputed update succeeded");
                        assert_eq!(
                            &expected, precomputed,
                            "precomputed note commitment trees must match the parent's trees \
                             updated with this block",
                        );
                    }
                    let tree_result = precomputed.map(|trees| note_commitment_trees = trees);
                    (tree_result, check_commitment())
                } else if shielded_commitments >= OVERLAP_COMMITMENT_THRESHOLD {
                    rayon::join(
                        || note_commitment_trees.update_trees_parallel(&block),
                        check_commitment,
                    )
                } else {
                    let tree_result = note_commitment_trees.update_trees_parallel(&block);
                    (tree_result, check_commitment())
                };
                // Keep tree errors ahead of commitment errors, regardless of completion order.
                tree_result.map_err(ValidateContextError::from)?;
                commitment_result?;
                // The tree update and the commitment check can overlap, so they share one phase.
                record_phase!(phase_timer, "checkpoint_trees_and_commitment");

                // Update the history tree.
                //
                // TODO: run this CPU-intensive cryptography in a parallel rayon
                // thread, if it shows up in profiles
                let history_tree_mut = Arc::make_mut(&mut history_tree);
                let sapling_root = note_commitment_trees.sapling.root();
                let orchard_root = note_commitment_trees.orchard.root();
                let ironwood_root = note_commitment_trees.ironwood.root();
                history_tree_mut
                    .push(
                        &self.network(),
                        block.clone(),
                        BlockCommitmentTreeRoots {
                            sapling: &sapling_root,
                            orchard: &orchard_root,
                            ironwood: &ironwood_root,
                        },
                    )
                    .map_err(Arc::new)
                    .map_err(ValidateContextError::from)?;
                record_phase!(phase_timer, "checkpoint_history_tree_push");

                let treestate = Treestate {
                    note_commitment_trees,
                    history_tree,
                };

                let height = checkpoint_verified.height;
                parent_value_pool =
                    cached_value_pool.unwrap_or_else(|| self.db.finalized_value_pool());
                let deferred_pool_balance_change = calculate_deferred_pool_balance_change(
                    height,
                    &self.network(),
                    parent_value_pool,
                )
                .map_err(|subsidy_error| ValidateContextError::InvalidSubsidy {
                    subsidy_error: subsidy_error.into(),
                    height,
                    block_hash: checkpoint_verified.hash,
                })?;
                record_phase!(phase_timer, "checkpoint_value_pool");

                (
                    height,
                    checkpoint_verified.hash,
                    FinalizedBlock::from_checkpoint_verified(
                        checkpoint_verified,
                        treestate,
                        deferred_pool_balance_change,
                    ),
                    Some(prev_note_commitment_trees),
                )
            }
            FinalizableBlock::Contextual {
                contextually_verified,
                treestate,
            } => {
                let height = contextually_verified.height;
                parent_value_pool = self.db.finalized_value_pool();
                let deferred_pool_balance_change = calculate_deferred_pool_balance_change(
                    height,
                    &self.network(),
                    parent_value_pool,
                )
                .map_err(|subsidy_error| ValidateContextError::InvalidSubsidy {
                    subsidy_error: subsidy_error.into(),
                    height,
                    block_hash: contextually_verified.hash,
                })?;

                (
                    height,
                    contextually_verified.hash,
                    FinalizedBlock::from_contextually_verified(
                        contextually_verified,
                        treestate,
                        deferred_pool_balance_change,
                    ),
                    prev_note_commitment_trees,
                )
            }
        };
        record_phase!(phase_timer, "checkpoint_finalized_block");

        let (committed_tip_height, committed_tip_hash, is_empty) = match parent_tip {
            Some((height, hash)) => (Some(height), hash, false),
            None => (
                self.db.finalized_tip_height(),
                self.db.finalized_tip_hash(),
                self.db.is_empty(),
            ),
        };

        // Assert that callers (including unit tests) get the chain order correct
        if is_empty {
            assert_eq!(
                committed_tip_hash, finalized.block.header.previous_block_hash,
                "the first block added to an empty state must be a genesis block, source: {source}",
            );
            assert_eq!(
                block::Height(0),
                height,
                "cannot commit genesis: invalid height, source: {source}",
            );
        } else {
            assert_eq!(
                committed_tip_height.expect("state must have a genesis block committed") + 1,
                Some(height),
                "committed block height must be 1 more than the finalized tip height, source: {source}",
            );

            assert_eq!(
                committed_tip_hash, finalized.block.header.previous_block_hash,
                "committed block must be a child of the finalized tip, source: {source}",
            );
        }

        record_phase!(phase_timer, "checkpoint_tip_checks");

        let note_commitment_trees = finalized.treestate.note_commitment_trees.clone();
        // An `Arc` clone: the block write drops its copy, so the next commit can update the tree
        // in place.
        let history_tree = use_tip_cache.then(|| finalized.treestate.history_tree.clone());

        let result = self.db.write_block(
            finalized,
            prev_note_commitment_trees,
            parent_value_pool,
            &self.network(),
            source,
            phase_timer,
        );

        if result.is_ok() {
            // TODO: move the stop height check to the syncer (#3442)
            if self.is_at_stop_height(height) {
                tracing::info!(
                    ?height,
                    ?hash,
                    block_source = ?source,
                    "stopping at configured height, flushing database to disk"
                );

                // We're just about to do a forced exit, so it's ok to do a forced db shutdown
                self.db.shutdown(true);

                // Drops tracing log output that's hasn't already been written to stdout
                // since this exits before calling drop on the WorkerGuard for the logger thread.
                // This is okay for now because this is test-only code
                //
                // TODO: Call ZebradApp.shutdown or drop its Tracing component before calling exit_process to flush logs to stdout
                Self::exit_process();
            }
        }

        result.map(|(hash, value_pool)| {
            let tip_cache = history_tree.map(|history_tree| CheckpointTipCache {
                tip: (height, hash),
                history_tree,
                value_pool,
            });

            (hash, note_commitment_trees, tip_cache)
        })
    }

    /// Stop the process if `block_height` is greater than or equal to the
    /// configured stop height.
    fn is_at_stop_height(&self, block_height: block::Height) -> bool {
        let debug_stop_at_height = match self.debug_stop_at_height {
            Some(debug_stop_at_height) => debug_stop_at_height,
            None => return false,
        };

        block_height >= debug_stop_at_height
    }

    /// Exit the host process.
    ///
    /// Designed for debugging and tests.
    ///
    /// TODO: move the stop height check to the syncer (#3442)
    fn exit_process() -> ! {
        tracing::info!("exiting Zebra");

        // Some OSes require a flush to send all output to the terminal.
        // Zebra's logging doesn't depend on `tokio`, so we flush the stdlib sync streams.
        //
        // TODO: if this doesn't work, send an empty line as well.
        let _ = stdout().lock().flush();
        let _ = stderr().lock().flush();

        // Give some time to logger thread to flush out any remaining lines to stdout
        // and yield so that tests pass on MacOS
        std::thread::sleep(std::time::Duration::from_secs(3));

        // Exits before calling drop on the WorkerGuard for the logger thread,
        // dropping any lines that haven't already been written to stdout.
        // This is okay for now because this is test-only code
        std::process::exit(0);
    }
}

/// Calculates deferred issuance and fixed lockbox disbursements using the exact parent's pools.
pub(crate) fn calculate_deferred_pool_balance_change(
    height: block::Height,
    network: &Network,
    parent_chain_value_pools: ValueBalance<NonNegative>,
) -> Result<DeferredPoolBalanceChange, SubsidyError> {
    zebra_chain::parameters::subsidy::funding_stream_values(
        height,
        network,
        block_subsidy_with_parent_pools(height, network, parent_chain_value_pools)?,
    )?
    .remove(&zebra_chain::parameters::subsidy::FundingStreamReceiver::Deferred)
    .unwrap_or_default()
    .checked_sub(network.lockbox_disbursement_total_amount(height))
    .map(DeferredPoolBalanceChange::new)
    .ok_or(SubsidyError::Underflow)
}
