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
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use zebra_chain::{block, parallel::tree::NoteCommitmentTrees, parameters::Network};
use zebra_db::{
    block::RetentionPlan,
    chain::BLOCK_INFO,
    transparent::{BALANCE_BY_TRANSPARENT_ADDR, TX_LOC_BY_SPENT_OUT_LOC},
};

use crate::{
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    error::CommitCheckpointVerifiedError,
    request::{FinalizableBlock, FinalizedBlock, Treestate},
    service::{check, QueuedCheckpointVerified},
    CheckpointVerifiedBlock, Config, ValidateContextError,
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
pub use zebra_db::prune::{
    preview_prune_finalized_state, prune_finalized_state, PruneFinalizedStateError,
    PruneFinalizedStateOptions, PruneFinalizedStateSummary,
};
pub use zebra_db::rollback::{
    preview_rollback_finalized_state, rollback_finalized_state, RollbackBackupSummary,
    RollbackFinalizedStateError, RollbackFinalizedStateOptions, RollbackFinalizedStateSummary,
};

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
    // Chain
    "history_tree",
    "tip_chain_value_pool",
    BLOCK_INFO,
    // Storage policy
    PRUNING_METADATA,
];

/// The name of the column family that records pruning progress.
///
/// In pruned storage mode this holds a single entry, keyed by the unit value
/// `()`, mapping to the next block height managed by online pruning. The
/// presence of this entry marks the database as pruned, which is a one-way state:
/// a pruned database cannot be reopened in archive mode.
pub const PRUNING_METADATA: &str = "pruning_metadata";

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

    /// The lowest checkpoint-verified block height whose raw transaction bytes
    /// should be retained during checkpoint sync in pruned mode.
    checkpoint_raw_tx_retention_start: Option<block::Height>,

    /// `true` if raw transactions from an archive-mode sync may still exist
    /// before `checkpoint_raw_tx_retention_start`.
    ///
    /// Shared via `Arc<AtomicBool>` because [`FinalizedState`] is `Clone` and the
    /// commit path mutates this flag (clearing it once the archive backlog is
    /// drained), so per the shared-state invariant below it must be shared across
    /// clones rather than an owned `bool`.
    checkpoint_raw_tx_archive_backlog: Arc<AtomicBool>,

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

    #[cfg(feature = "elasticsearch")]
    /// The elasticsearch handle.
    pub elastic_db: Option<elasticsearch::Elasticsearch>,

    #[cfg(feature = "elasticsearch")]
    /// A collection of blocks to be sent to elasticsearch as a bulk.
    pub elastic_blocks: Vec<String>,
}

impl FinalizedState {
    /// Returns an on-disk database instance for `config`, `network`, and `elastic_db`.
    /// If there is no existing database, creates a new database on disk.
    pub fn new(
        config: &Config,
        network: &Network,
        #[cfg(feature = "elasticsearch")] enable_elastic_db: bool,
    ) -> Self {
        Self::new_with_debug(
            config,
            network,
            false,
            #[cfg(feature = "elasticsearch")]
            enable_elastic_db,
            false,
        )
    }

    /// Returns an on-disk database instance with the supplied production and debug settings.
    /// If there is no existing database, creates a new database on disk.
    ///
    /// This method is intended for use in tests.
    pub(crate) fn new_with_debug(
        config: &Config,
        network: &Network,
        debug_skip_format_upgrades: bool,
        #[cfg(feature = "elasticsearch")] enable_elastic_db: bool,
        read_only: bool,
    ) -> Self {
        Self::new_with_debug_and_storage_validation(
            config,
            network,
            debug_skip_format_upgrades,
            #[cfg(feature = "elasticsearch")]
            enable_elastic_db,
            read_only,
            true,
        )
    }

    /// Returns an on-disk database instance with storage mode validation disabled.
    ///
    /// This method is intended for tests that use intentionally invalid storage
    /// configuration values to exercise lower-level pruning behavior.
    #[cfg(test)]
    pub(crate) fn new_with_debug_without_storage_validation(
        config: &Config,
        network: &Network,
        debug_skip_format_upgrades: bool,
        #[cfg(feature = "elasticsearch")] enable_elastic_db: bool,
        read_only: bool,
    ) -> Self {
        Self::new_with_debug_and_storage_validation(
            config,
            network,
            debug_skip_format_upgrades,
            #[cfg(feature = "elasticsearch")]
            enable_elastic_db,
            read_only,
            false,
        )
    }

    fn new_with_debug_and_storage_validation(
        config: &Config,
        network: &Network,
        debug_skip_format_upgrades: bool,
        #[cfg(feature = "elasticsearch")] enable_elastic_db: bool,
        read_only: bool,
        validate_storage_mode: bool,
    ) -> Self {
        // Fail fast on an invalid storage configuration, before opening the database.
        if validate_storage_mode {
            if let Err(error) = config.validate_storage_mode(network) {
                panic!("{error}");
            }
        }

        #[cfg(feature = "elasticsearch")]
        let elastic_db = if enable_elastic_db {
            use elasticsearch::{
                auth::Credentials::Basic,
                cert::CertificateValidation,
                http::transport::{SingleNodeConnectionPool, TransportBuilder},
                http::Url,
                Elasticsearch,
            };

            let conn_pool = SingleNodeConnectionPool::new(
                Url::parse(config.elasticsearch_url.as_str())
                    .expect("configured elasticsearch url is invalid"),
            );
            let transport = TransportBuilder::new(conn_pool)
                .cert_validation(CertificateValidation::None)
                .auth(Basic(
                    config.clone().elasticsearch_username,
                    config.clone().elasticsearch_password,
                ))
                .build()
                .expect("elasticsearch transport builder should not fail");

            Some(Elasticsearch::new(transport))
        } else {
            None
        };

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
        );

        #[cfg(feature = "elasticsearch")]
        let new_state = Self {
            debug_stop_at_height: config.debug_stop_at_height.map(block::Height),
            checkpoint_raw_tx_retention_start: None,
            checkpoint_raw_tx_archive_backlog: Arc::new(AtomicBool::new(false)),
            db,
            elastic_db,
            elastic_blocks: vec![],
        };

        #[cfg(not(feature = "elasticsearch"))]
        let new_state = Self {
            debug_stop_at_height: config.debug_stop_at_height.map(block::Height),
            checkpoint_raw_tx_retention_start: None,
            checkpoint_raw_tx_archive_backlog: Arc::new(AtomicBool::new(false)),
            db,
        };

        // Pruning is a one-way storage mode. Refuse to open a database that has
        // already pruned historical data in archive mode, because the data it
        // would be expected to serve has been irreversibly deleted.
        if config.pruning_config().is_none() && new_state.db.is_pruned() {
            panic!(
                "this database has been pruned and cannot be opened in archive storage mode; \
                 configure pruned storage mode (`storage_mode.pruned`), or delete the cache \
                 directory and re-sync from genesis"
            );
        }

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

        new_state
    }

    /// Configure checkpoint raw transaction retention for pruned checkpoint sync.
    ///
    /// Checkpoint-verified blocks before the configured start can skip `tx_by_loc`
    /// writes, because they are outside the retention window relative to the
    /// known final checkpoint target.
    pub(crate) fn with_checkpoint_raw_tx_retention(
        mut self,
        max_checkpoint_height: block::Height,
        config: &Config,
    ) -> Self {
        self.checkpoint_raw_tx_retention_start = config.pruning_config().and_then(|pruning| {
            compute_checkpoint_raw_tx_retention_start(max_checkpoint_height, pruning.tx_retention)
        });

        let has_archive_backlog = config.pruning_config().is_some()
            && self.checkpoint_raw_tx_retention_start.is_some_and(|start| {
                let prune_from = self.db.lowest_retained_height().unwrap_or(block::Height(1));

                self.db.raw_transactions_exist_in_range(prune_from, start)
            });

        self.checkpoint_raw_tx_archive_backlog
            .store(has_archive_backlog, Ordering::Relaxed);

        self
    }

    /// Returns `true` when raw transaction bytes should be stored for a
    /// checkpoint-verified block at `height`.
    fn store_checkpoint_raw_transactions(&self, height: block::Height) -> bool {
        height.is_min()
            || self
                .checkpoint_raw_tx_retention_start
                .is_none_or(|start| height >= start)
    }

    /// Resolves the [`RetentionPlan`] for committing the finalized block at
    /// `height` in the current storage mode.
    ///
    /// This is the single place the raw-transaction retention decision is made:
    /// whether to write this block's raw transactions, which aged-out or backlog
    /// range to delete, and how to advance the pruning marker.
    /// [`ZebraDb::write_block`] applies the returned plan without re-deriving it.
    ///
    /// `is_checkpoint` selects the checkpoint-sync policy (a retention start
    /// before which raw transactions are skipped, plus bounded archive-backlog
    /// draining) rather than the near-tip policy (ordinary online pruning). In
    /// archive mode the plan is always [`RetentionPlan::Store`].
    fn retention_plan(&self, height: block::Height, is_checkpoint: bool) -> RetentionPlan {
        let Some(pruning) = self.db.config().pruning_config() else {
            return RetentionPlan::Store;
        };

        let lowest_retained = self.db.lowest_retained_height();

        // Checkpoint blocks before the retention start: skip raw transactions,
        // draining any pre-existing archive backlog in bounded chunks first.
        if is_checkpoint && !self.store_checkpoint_raw_transactions(height) {
            let skipped_until = (height + 1).expect("checkpoint block height plus one is valid");

            if self
                .checkpoint_raw_tx_archive_backlog
                .load(Ordering::Relaxed)
            {
                if let Some((from, until)) = self
                    .db
                    .checkpoint_raw_transaction_prune_range(skipped_until)
                {
                    // The marker can only advance past this block once the
                    // backlog below it is fully drained, so the last chunk (which
                    // reaches `skipped_until`) is the one that skips this block's
                    // raw transactions and clears the backlog flag afterwards.
                    let final_chunk =
                        !checkpoint_prune_range_retains_current_height(height, Some((from, until)));

                    return RetentionPlan::DrainBacklog {
                        from,
                        until,
                        final_chunk,
                    };
                }
            }

            // No archive backlog left to drain: skip this block's raw
            // transactions and advance the pruning marker so readers know raw
            // data below it may be unavailable.
            return RetentionPlan::Skip {
                lowest_retained: skipped_until,
                write_marker: lowest_retained < Some(skipped_until),
            };
        }

        // Archive-equivalent path for this block (contextual blocks, or
        // checkpoint blocks within the retention window): keep raw transactions
        // and run ordinary online pruning.
        match ZebraDb::prune_height_range(height, pruning.tx_retention, lowest_retained) {
            Some((from, until)) => RetentionPlan::Prune { from, until },
            None => RetentionPlan::Store,
        }
    }

    /// Returns `true` if the cached archive raw transaction backlog flag is set.
    #[cfg(test)]
    pub(crate) fn has_checkpoint_raw_tx_archive_backlog(&self) -> bool {
        self.checkpoint_raw_tx_archive_backlog
            .load(Ordering::Relaxed)
    }

    /// Returns the configured network for this database.
    pub fn network(&self) -> Network {
        self.db.network()
    }

    /// Commit a checkpoint-verified block to the state.
    ///
    /// It's the caller's responsibility to ensure that blocks are committed in
    /// order.
    pub fn commit_finalized(
        &mut self,
        ordered_block: QueuedCheckpointVerified,
        prev_note_commitment_trees: Option<NoteCommitmentTrees>,
    ) -> Result<(CheckpointVerifiedBlock, NoteCommitmentTrees), CommitCheckpointVerifiedError> {
        let (checkpoint_verified, rsp_tx) = ordered_block;
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
        let (height, hash, finalized, prev_note_commitment_trees, retention) =
            match finalizable_block {
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

                    // Update the note commitment trees.
                    let mut note_commitment_trees = prev_note_commitment_trees.clone();
                    note_commitment_trees
                        .update_trees_parallel(&block)
                        .map_err(ValidateContextError::from)?;

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
                        block.clone(),
                        &self.network(),
                        &history_tree,
                    )?;

                    // Update the history tree.
                    //
                    // TODO: run this CPU-intensive cryptography in a parallel rayon
                    // thread, if it shows up in profiles
                    let history_tree_mut = Arc::make_mut(&mut history_tree);
                    let sapling_root = note_commitment_trees.sapling.root();
                    let orchard_root = note_commitment_trees.orchard.root();
                    history_tree_mut
                        .push(&self.network(), block.clone(), &sapling_root, &orchard_root)
                        .map_err(Arc::new)
                        .map_err(ValidateContextError::from)?;

                    let treestate = Treestate {
                        note_commitment_trees,
                        history_tree,
                    };

                    let height = checkpoint_verified.height;
                    let hash = checkpoint_verified.hash;

                    (
                        height,
                        hash,
                        FinalizedBlock::from_checkpoint_verified(checkpoint_verified, treestate),
                        Some(prev_note_commitment_trees),
                        self.retention_plan(height, true),
                    )
                }
                FinalizableBlock::Contextual {
                    contextually_verified,
                    treestate,
                } => {
                    let height = contextually_verified.height;

                    (
                        height,
                        contextually_verified.hash,
                        FinalizedBlock::from_contextually_verified(
                            contextually_verified,
                            treestate,
                        ),
                        prev_note_commitment_trees,
                        self.retention_plan(height, false),
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

        #[cfg(feature = "elasticsearch")]
        let finalized_inner_block = finalized.block.clone();
        let note_commitment_trees = finalized.treestate.note_commitment_trees.clone();

        let result = self.db.write_block(
            finalized,
            prev_note_commitment_trees,
            &self.network(),
            source,
            retention,
        );

        if result.is_ok() {
            if retention.clears_archive_backlog() {
                self.checkpoint_raw_tx_archive_backlog
                    .store(false, Ordering::Relaxed);
            }

            // Save blocks to elasticsearch if the feature is enabled.
            #[cfg(feature = "elasticsearch")]
            self.elasticsearch(&finalized_inner_block);

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

    #[cfg(feature = "elasticsearch")]
    /// Store finalized blocks into an elasticsearch database.
    ///
    /// We use the elasticsearch bulk api to index multiple blocks at a time while we are
    /// synchronizing the chain, when we get close to tip we index blocks one by one.
    pub fn elasticsearch(&mut self, block: &Arc<block::Block>) {
        if let Some(client) = self.elastic_db.clone() {
            let block_time = block.header.time.timestamp();
            let local_time = chrono::Utc::now().timestamp();

            // Bulk size is small enough to avoid the elasticsearch 100mb content length limitation.
            // MAX_BLOCK_BYTES = 2MB but each block use around 4.1 MB of JSON.
            // Each block count as 2 as we send them with a operation/header line. A value of 48
            // is 24 blocks.
            const AWAY_FROM_TIP_BULK_SIZE: usize = 48;

            // The number of blocks the bulk will have when we are in sync.
            // A value of 2 means only 1 block as we want to insert them as soon as we get
            // them for a real time experience. This is the same for mainnet and testnet.
            const CLOSE_TO_TIP_BULK_SIZE: usize = 2;

            // We consider in sync when the local time and the blockchain time difference is
            // less than this number of seconds.
            const CLOSE_TO_TIP_SECONDS: i64 = 14400; // 4 hours

            let mut blocks_size_to_dump = AWAY_FROM_TIP_BULK_SIZE;

            // If we are close to the tip, index one block per bulk call.
            if local_time - block_time < CLOSE_TO_TIP_SECONDS {
                blocks_size_to_dump = CLOSE_TO_TIP_BULK_SIZE;
            }

            // Insert the operation line.
            let height_number = block.coinbase_height().unwrap_or(block::Height(0)).0;
            self.elastic_blocks.push(
                serde_json::json!({
                    "index": {
                        "_id": height_number.to_string().as_str()
                    }
                })
                .to_string(),
            );

            // Insert the block itself.
            self.elastic_blocks
                .push(serde_json::json!(block).to_string());

            // We are in bulk time, insert to ES all we have.
            if self.elastic_blocks.len() >= blocks_size_to_dump {
                let rt = tokio::runtime::Runtime::new()
                    .expect("runtime creation for elasticsearch should not fail.");
                let blocks = self.elastic_blocks.clone();
                let network = self.network();

                rt.block_on(async move {
                    // Send a ping to the server to check if it is available before inserting.
                    if client.ping().send().await.is_err() {
                        tracing::error!("Elasticsearch is not available, skipping block indexing");
                        return;
                    }

                    let response = client
                        .bulk(elasticsearch::BulkParts::Index(
                            format!("zcash_{}", network.to_string().to_lowercase()).as_str(),
                        ))
                        .body(blocks)
                        .send()
                        .await
                        .expect("ES Request should never fail");

                    // Make sure no errors ever.
                    let response_body = response
                        .json::<serde_json::Value>()
                        .await
                        .expect("ES response parsing error. Maybe we are sending more than 100 mb of data (`http.max_content_length`)");
                    let errors = response_body["errors"].as_bool().unwrap_or(true);
                    assert!(!errors, "{}", format!("ES error: {response_body}"));
                });

                // Clean the block storage.
                self.elastic_blocks.clear();
            }
        }
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

/// Returns `true` when archive-backlog pruning does not prune the current
/// checkpoint block, so its raw transactions still need to be written.
fn checkpoint_prune_range_retains_current_height(
    height: block::Height,
    checkpoint_prune_range: Option<(block::Height, block::Height)>,
) -> bool {
    checkpoint_prune_range.is_some_and(|(_, prune_until)| prune_until <= height)
}

/// Returns the lowest checkpoint height whose raw transactions should be kept.
fn compute_checkpoint_raw_tx_retention_start(
    max_checkpoint_height: block::Height,
    tx_retention: u32,
) -> Option<block::Height> {
    let max_skipped_height = max_checkpoint_height.0.checked_sub(tx_retention)?;

    max_skipped_height.checked_add(1).map(block::Height)
}
