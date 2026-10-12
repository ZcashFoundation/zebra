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
};

use zebra_chain::{
    amount::{DeferredPoolBalanceChange, NonNegative},
    block,
    parallel::tree::NoteCommitmentTrees,
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
    service::{check, QueuedCheckpointVerified},
    CheckpointVerifiedBlock, Config, StateInitError, ValidateContextError,
};

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
    /// It's the caller's responsibility to ensure that blocks are committed in
    /// order.
    /// Runs on the blocking state writer; mined commit notifications precede the response.
    /// The caller must reserve `committed_permit` for mined blocks before calling this method.
    pub fn commit_finalized(
        &mut self,
        ordered_block: QueuedCheckpointVerified,
        prev_note_commitment_trees: Option<NoteCommitmentTrees>,
        committed_permit: Option<tokio::sync::mpsc::OwnedPermit<(block::Hash, block::Height)>>,
    ) -> Result<(CheckpointVerifiedBlock, NoteCommitmentTrees), CommitCheckpointVerifiedError> {
        let (checkpoint_verified, rsp_tx, _) = ordered_block;
        let result = self.commit_finalized_direct(
            checkpoint_verified.clone().into(),
            prev_note_commitment_trees,
            "commit checkpoint-verified request",
        );

        if result.is_ok() {
            metrics::counter!("state.checkpoint.finalized.block.count").increment(1);
            metrics::gauge!("state.checkpoint.finalized.block.height")
                .set(checkpoint_verified.height.0 as f64);

            // This height gauge is updated for both fully verified and checkpoint blocks.
            // These updates can't conflict, because the state makes sure that blocks
            // are committed in order.
            metrics::gauge!("zcash.chain.verified.block.height")
                .set(checkpoint_verified.height.0 as f64);
            metrics::counter!("zcash.chain.verified.block.total").increment(1);
            if let Some(permit) = committed_permit {
                permit.send((checkpoint_verified.hash, checkpoint_verified.height));
            }
        } else {
            metrics::counter!("state.checkpoint.error.block.count").increment(1);
            metrics::gauge!("state.checkpoint.error.block.height")
                .set(checkpoint_verified.height.0 as f64);
        };

        let _ = rsp_tx.send(result.clone().map(|(hash, _)| hash));

        result.map(|(_hash, note_commitment_trees)| (checkpoint_verified, note_commitment_trees))
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
    #[allow(clippy::unwrap_in_result)]
    pub fn commit_finalized_direct(
        &mut self,
        finalizable_block: FinalizableBlock,
        prev_note_commitment_trees: Option<NoteCommitmentTrees>,
        source: &str,
    ) -> Result<(block::Hash, NoteCommitmentTrees), CommitCheckpointVerifiedError> {
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
                let mut history_tree = self.db.history_tree();
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
                let (tree_result, commitment_result) =
                    if shielded_commitments >= OVERLAP_COMMITMENT_THRESHOLD {
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

                let treestate = Treestate {
                    note_commitment_trees,
                    history_tree,
                };

                let height = checkpoint_verified.height;
                let deferred_pool_balance_change = calculate_deferred_pool_balance_change(
                    height,
                    &self.network(),
                    self.db.finalized_value_pool(),
                )
                .map_err(|subsidy_error| ValidateContextError::InvalidSubsidy {
                    subsidy_error: subsidy_error.into(),
                    height,
                    block_hash: checkpoint_verified.hash,
                })?;

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
                let deferred_pool_balance_change = calculate_deferred_pool_balance_change(
                    height,
                    &self.network(),
                    self.db.finalized_value_pool(),
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

        let committed_tip_hash = self.db.finalized_tip_hash();
        let committed_tip_height = self.db.finalized_tip_height();

        // Assert that callers (including unit tests) get the chain order correct
        if self.db.is_empty() {
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

        let note_commitment_trees = finalized.treestate.note_commitment_trees.clone();

        let result = self.db.write_block(
            finalized,
            prev_note_commitment_trees,
            &self.network(),
            source,
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

        result.map(|hash| (hash, note_commitment_trees))
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
