//! The primary implementation of the `zebra_state::Service` built upon rocksdb.
//!
//! Zebra's database is implemented in 4 layers:
//! - [`FinalizedState`]: queues, validates, and commits blocks, using...
//! - [`zebra_db`]: reads and writes [`zebra_chain`] types to the database, using...
//! - [`disk_db`]: reads and writes format-specific types to the database, using...
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
    collections::HashMap,
    io::{stderr, stdout, Write},
    path::Path,
    sync::Arc,
};

use zebra_chain::{
    block::{self, Block},
    history_tree::HistoryTree,
    parameters::{Network, GENESIS_PREVIOUS_BLOCK_HASH},
};

use crate::{
    service::{check, finalized_state::disk_db::DiskDb, QueuedFinalized},
    BoxError, Config, FinalizedBlock,
};

mod disk_db;
mod disk_format;
mod zebra_db;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

#[cfg(test)]
mod tests;

/// The finalized part of the chain state, stored in the db.
pub struct FinalizedState {
    /// The underlying database.
    db: DiskDb,

    /// Queued blocks that arrived out of order, indexed by their parent block hash.
    queued_by_prev_hash: HashMap<block::Hash, QueuedFinalized>,

    /// A metric tracking the maximum height that's currently in `queued_by_prev_hash`
    ///
    /// Set to `f64::NAN` if `queued_by_prev_hash` is empty, because grafana shows NaNs
    /// as a break in the graph.
    max_queued_height: f64,

    /// The configured stop height.
    ///
    /// Commit blocks to the finalized state up to this height, then exit Zebra.
    debug_stop_at_height: Option<block::Height>,

    /// The configured network.
    network: Network,
}

impl FinalizedState {
    pub fn new(config: &Config, network: Network) -> Self {
        let db = DiskDb::new(config, network);

        let new_state = Self {
            queued_by_prev_hash: HashMap::new(),
            max_queued_height: f64::NAN,
            db,
            debug_stop_at_height: config.debug_stop_at_height.map(block::Height),
            network,
        };

        if let Some(tip_height) = new_state.finalized_tip_height() {
            if new_state.is_at_stop_height(tip_height) {
                let debug_stop_at_height = new_state
                    .debug_stop_at_height
                    .expect("true from `is_at_stop_height` implies `debug_stop_at_height` is Some");
                let tip_hash = new_state.finalized_tip_hash();

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

                Self::exit_process();
            }
        }

        tracing::info!(tip = ?new_state.tip(), "loaded Zebra state cache");

        new_state
    }

    /// Returns the `Path` where the files used by this database are located.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        self.db.path()
    }

    /// Returns the hash of the current finalized tip block.
    pub fn finalized_tip_hash(&self) -> block::Hash {
        self.tip()
            .map(|(_, hash)| hash)
            // if the state is empty, return the genesis previous block hash
            .unwrap_or(GENESIS_PREVIOUS_BLOCK_HASH)
    }

    /// Returns the height of the current finalized tip block.
    pub fn finalized_tip_height(&self) -> Option<block::Height> {
        self.tip().map(|(height, _)| height)
    }

    /// Returns the tip block, if there is one.
    pub fn tip_block(&self) -> Option<Arc<Block>> {
        let (height, _hash) = self.tip()?;
        self.block(height.into())
    }

    /// Queue a finalized block to be committed to the state.
    ///
    /// After queueing a finalized block, this method checks whether the newly
    /// queued block (and any of its descendants) can be committed to the state.
    ///
    /// Returns the highest finalized tip block committed from the queue,
    /// or `None` if no blocks were committed in this call.
    /// (Use [`tip_block`] to get the finalized tip, regardless of when it was committed.)
    pub fn queue_and_commit_finalized(
        &mut self,
        queued: QueuedFinalized,
    ) -> Option<FinalizedBlock> {
        let mut highest_queue_commit = None;

        let prev_hash = queued.0.block.header.previous_block_hash;
        let height = queued.0.height;
        self.queued_by_prev_hash.insert(prev_hash, queued);

        while let Some(queued_block) = self.queued_by_prev_hash.remove(&self.finalized_tip_hash()) {
            if let Ok(finalized) = self.commit_finalized(queued_block) {
                highest_queue_commit = Some(finalized);
            } else {
                // the last block in the queue failed, so we can't commit the next block
                break;
            }
        }

        if self.queued_by_prev_hash.is_empty() {
            self.max_queued_height = f64::NAN;
        } else if self.max_queued_height.is_nan() || self.max_queued_height < height.0 as _ {
            // if there are still blocks in the queue, then either:
            //   - the new block was lower than the old maximum, and there was a gap before it,
            //     so the maximum is still the same (and we skip this code), or
            //   - the new block is higher than the old maximum, and there is at least one gap
            //     between the finalized tip and the new maximum
            self.max_queued_height = height.0 as _;
        }

        metrics::gauge!("state.checkpoint.queued.max.height", self.max_queued_height);
        metrics::gauge!(
            "state.checkpoint.queued.block.count",
            self.queued_by_prev_hash.len() as f64
        );

        highest_queue_commit
    }

    /// Commit a finalized block to the state.
    ///
    /// It's the caller's responsibility to ensure that blocks are committed in
    /// order. This function is called by [`queue`], which ensures order.
    /// It is intentionally not exposed as part of the public API of the
    /// [`FinalizedState`].
    fn commit_finalized(&mut self, queued_block: QueuedFinalized) -> Result<FinalizedBlock, ()> {
        let (finalized, rsp_tx) = queued_block;
        let result = self.commit_finalized_direct(finalized.clone(), "CommitFinalized request");

        let block_result = if result.is_ok() {
            metrics::counter!("state.checkpoint.finalized.block.count", 1);
            metrics::gauge!(
                "state.checkpoint.finalized.block.height",
                finalized.height.0 as _
            );

            // This height gauge is updated for both fully verified and checkpoint blocks.
            // These updates can't conflict, because the state makes sure that blocks
            // are committed in order.
            metrics::gauge!("zcash.chain.verified.block.height", finalized.height.0 as _);
            metrics::counter!("zcash.chain.verified.block.total", 1);

            Ok(finalized)
        } else {
            metrics::counter!("state.checkpoint.error.block.count", 1);
            metrics::gauge!(
                "state.checkpoint.error.block.height",
                finalized.height.0 as _
            );

            Err(())
        };

        let _ = rsp_tx.send(result.map_err(Into::into));

        block_result
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
        finalized: FinalizedBlock,
        source: &str,
    ) -> Result<block::Hash, BoxError> {
        let committed_tip_hash = self.finalized_tip_hash();
        let committed_tip_height = self.finalized_tip_height();

        // Assert that callers (including unit tests) get the chain order correct
        if self.is_empty() {
            assert_eq!(
                committed_tip_hash, finalized.block.header.previous_block_hash,
                "the first block added to an empty state must be a genesis block, source: {}",
                source,
            );
            assert_eq!(
                block::Height(0),
                finalized.height,
                "cannot commit genesis: invalid height, source: {}",
                source,
            );
        } else {
            assert_eq!(
                committed_tip_height.expect("state must have a genesis block committed") + 1,
                Some(finalized.height),
                "committed block height must be 1 more than the finalized tip height, source: {}",
                source,
            );

            assert_eq!(
                committed_tip_hash, finalized.block.header.previous_block_hash,
                "committed block must be a child of the finalized tip, source: {}",
                source,
            );
        }

        // Check the block commitment. For Nu5-onward, the block hash commits only
        // to non-authorizing data (see ZIP-244). This checks the authorizing data
        // commitment, making sure the entire block contents were committed to.
        // The test is done here (and not during semantic validation) because it needs
        // the history tree root. While it _is_ checked during contextual validation,
        // that is not called by the checkpoint verifier, and keeping a history tree there
        // would be harder to implement.
        let history_tree = self.history_tree();
        check::finalized_block_commitment_is_valid_for_chain_history(
            &finalized,
            self.network,
            &history_tree,
        )?;

        let finalized_height = finalized.height;
        let finalized_hash = finalized.hash;

        let result = self.write_block(finalized, history_tree, source);

        // TODO: move the stop height check to the syncer (#3442)
        if result.is_ok() && self.is_at_stop_height(finalized_height) {
            tracing::info!(
                height = ?finalized_height,
                hash = ?finalized_hash,
                block_source = ?source,
                "stopping at configured height, flushing database to disk"
            );

            self.db.shutdown();

            Self::exit_process();
        }

        result
    }

    /// Write `finalized` to the finalized state.
    ///
    /// Uses:
    /// - `history_tree`: the current tip's history tree
    /// - `source`: the source of the block in log messages
    ///
    /// # Errors
    ///
    /// - Propagates any errors from writing to the DB
    /// - Propagates any errors from updating history and note commitment trees
    fn write_block(
        &mut self,
        finalized: FinalizedBlock,
        history_tree: HistoryTree,
        source: &str,
    ) -> Result<block::Hash, BoxError> {
        let finalized_hash = finalized.hash;

        let all_utxos_spent_by_block = finalized
            .block
            .transactions
            .iter()
            .flat_map(|tx| tx.inputs().iter())
            .flat_map(|input| input.outpoint())
            .flat_map(|outpoint| self.utxo(&outpoint).map(|utxo| (outpoint, utxo)))
            .collect();

        let mut batch = disk_db::DiskWriteBatch::new();

        // In case of errors, propagate and do not write the batch.
        batch.prepare_block_batch(
            &self.db,
            finalized,
            self.network,
            all_utxos_spent_by_block,
            self.note_commitment_trees(),
            history_tree,
            self.finalized_value_pool(),
        )?;

        self.db.write(batch)?;

        tracing::trace!(?source, "committed block from");

        Ok(finalized_hash)
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

        std::process::exit(0);
    }
}
