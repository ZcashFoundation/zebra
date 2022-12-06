//! The primary implementation of the `zebra_state::Service` built upon rocksdb.
//!
//! Zebra's database is implemented in 4 layers:
//! - [`FinalizedState`]: queues, validates, and commits blocks, using...
//! - [`ZebraDb`]: reads and writes [`zebra_chain`] types to the database, using...
//! - [`DiskDb`](disk_db::DiskDb): reads and writes format-specific types
//!   to the database, using...
//! - [`disk_format`]: converts types to raw database bytes.
//!
//! These layers allow us to split [`zebra_chain`] types for efficient database storage.
//! They reduce the risk of data corruption bugs, runtime inconsistencies, and panics.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::{
    io::{stderr, stdout, Write},
    sync::Arc,
};

use zebra_chain::{block, parameters::Network};

use crate::{
    request::FinalizedWithTrees,
    service::{check, QueuedFinalized},
    BoxError, CloneError, Config, FinalizedBlock,
};

mod disk_db;
mod disk_format;
mod zebra_db;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

#[cfg(test)]
mod tests;

pub use disk_format::{OutputIndex, OutputLocation, TransactionLocation};

pub(super) use zebra_db::ZebraDb;

/// The finalized part of the chain state, stored in the db.
///
/// `rocksdb` allows concurrent writes through a shared reference,
/// so clones of the finalized state represent the same database instance.
/// When the final clone is dropped, the database is closed.
///
/// This is different from `NonFinalizedState::clone()`,
/// which returns an independent copy of the chains.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedState {
    // Configuration
    //
    // This configuration cannot be modified after the database is initialized,
    // because some clones would have different values.
    //
    /// The configured network.
    network: Network,

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
    pub fn new(config: &Config, network: Network) -> Self {
        let db = ZebraDb::new(config, network);

        let new_state = Self {
            network,
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

        tracing::info!(tip = ?new_state.db.tip(), "loaded Zebra state cache");

        new_state
    }

    /// Returns the configured network for this database.
    pub fn network(&self) -> Network {
        self.network
    }

    /// Commit a finalized block to the state.
    ///
    /// It's the caller's responsibility to ensure that blocks are committed in
    /// order.
    pub fn commit_finalized(
        &mut self,
        ordered_block: QueuedFinalized,
    ) -> Result<FinalizedBlock, BoxError> {
        let (finalized, rsp_tx) = ordered_block;
        let result =
            self.commit_finalized_direct(finalized.clone().into(), "CommitFinalized request");

        if result.is_ok() {
            metrics::counter!("state.checkpoint.finalized.block.count", 1);
            metrics::gauge!(
                "state.checkpoint.finalized.block.height",
                finalized.height.0 as f64,
            );

            // This height gauge is updated for both fully verified and checkpoint blocks.
            // These updates can't conflict, because the state makes sure that blocks
            // are committed in order.
            metrics::gauge!(
                "zcash.chain.verified.block.height",
                finalized.height.0 as f64,
            );
            metrics::counter!("zcash.chain.verified.block.total", 1);
        } else {
            metrics::counter!("state.checkpoint.error.block.count", 1);
            metrics::gauge!(
                "state.checkpoint.error.block.height",
                finalized.height.0 as f64,
            );
        };

        // Make the error cloneable, so we can send it to the block verify future,
        // and the block write task.
        let result = result.map_err(CloneError::from);

        let _ = rsp_tx.send(result.clone().map_err(BoxError::from));

        result.map(|_hash| finalized).map_err(BoxError::from)
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
        finalized_with_trees: FinalizedWithTrees,
        source: &str,
    ) -> Result<block::Hash, BoxError> {
        let finalized = finalized_with_trees.finalized;
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
                finalized.height,
                "cannot commit genesis: invalid height, source: {source}",
            );
        } else {
            assert_eq!(
                committed_tip_height.expect("state must have a genesis block committed") + 1,
                Some(finalized.height),
                "committed block height must be 1 more than the finalized tip height, source: {source}",
            );

            assert_eq!(
                committed_tip_hash, finalized.block.header.previous_block_hash,
                "committed block must be a child of the finalized tip, source: {source}",
            );
        }

        let (history_tree, note_commitment_trees) = match finalized_with_trees.treestate {
            // If the treestate associated with the block was supplied, use it
            // without recomputing it.
            Some(ref treestate) => (
                treestate.history_tree.clone(),
                treestate.note_commitment_trees.clone(),
            ),
            // If the treestate was not supplied, retrieve a previous treestate
            // from the database, and update it for the block being committed.
            None => {
                let mut history_tree = self.db.history_tree();
                let mut note_commitment_trees = self.db.note_commitment_trees();

                // Update the note commitment trees.
                note_commitment_trees.update_trees_parallel(&finalized.block)?;

                // Check the block commitment if the history tree was not
                // supplied by the non-finalized state. Note that we don't do
                // this check for history trees supplied by the non-finalized
                // state because the non-finalized state checks the block
                // commitment.
                //
                // For Nu5-onward, the block hash commits only to
                // non-authorizing data (see ZIP-244). This checks the
                // authorizing data commitment, making sure the entire block
                // contents were committed to. The test is done here (and not
                // during semantic validation) because it needs the history tree
                // root. While it _is_ checked during contextual validation,
                // that is not called by the checkpoint verifier, and keeping a
                // history tree there would be harder to implement.
                //
                // TODO: run this CPU-intensive cryptography in a parallel rayon
                // thread, if it shows up in profiles
                check::block_commitment_is_valid_for_chain_history(
                    finalized.block.clone(),
                    self.network,
                    &history_tree,
                )?;

                // Update the history tree.
                //
                // TODO: run this CPU-intensive cryptography in a parallel rayon
                // thread, if it shows up in profiles
                let history_tree_mut = Arc::make_mut(&mut history_tree);
                let sapling_root = note_commitment_trees.sapling.root();
                let orchard_root = note_commitment_trees.orchard.root();
                history_tree_mut.push(
                    self.network(),
                    finalized.block.clone(),
                    sapling_root,
                    orchard_root,
                )?;

                (history_tree, note_commitment_trees)
            }
        };

        let finalized_height = finalized.height;
        let finalized_hash = finalized.hash;

        let result = self.db.write_block(
            finalized,
            history_tree,
            note_commitment_trees,
            self.network,
            source,
        );

        // TODO: move the stop height check to the syncer (#3442)
        if result.is_ok() && self.is_at_stop_height(finalized_height) {
            tracing::info!(
                height = ?finalized_height,
                hash = ?finalized_hash,
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

        result
    }

    /// Stop the process if `block_height` is greater than or equal to the
    /// configured stop height.
    fn is_at_stop_height(&self, block_height: block::Height) -> bool {
        let debug_stop_at_height = match self.debug_stop_at_height {
            Some(debug_stop_at_height) => debug_stop_at_height,
            None => return false,
        };

        if block_height < debug_stop_at_height {
            return false;
        }

        true
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
