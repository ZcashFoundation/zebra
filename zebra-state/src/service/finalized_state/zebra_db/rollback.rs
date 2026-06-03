//! Offline rollback support for the finalized state database.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use semver::Version;
use zebra_chain::{
    amount::{self, Amount, DeferredPoolBalanceChange, NonNegative},
    block::{self, Block, Height},
    history_tree::{HistoryTree, HistoryTreeError},
    parallel::tree::{NoteCommitmentTreeError, NoteCommitmentTrees},
    parameters::{
        subsidy::{block_subsidy, funding_stream_values, FundingStreamReceiver, SubsidyError},
        Network, NetworkUpgrade,
    },
    subtree::NoteCommitmentSubtreeIndex,
    transaction,
    transparent::{self, Input},
    value_balance::ValueBalance,
};

use crate::{
    config::state_database_format_version_on_disk,
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    service::{
        finalized_state::{
            disk_db::{DiskWriteBatch, WriteDisk},
            disk_format::{
                transparent::{
                    AddressBalanceLocation, AddressTransaction, AddressUnspentOutput,
                    OutputLocation,
                },
                TransactionLocation,
            },
            zebra_db::{
                chain::BLOCK_INFO,
                transparent::{BALANCE_BY_TRANSPARENT_ADDR, TX_LOC_BY_SPENT_OUT_LOC},
                ZebraDb,
            },
            STATE_COLUMN_FAMILIES_IN_CODE,
        },
        non_finalized_state::write_semantically_verified_backup_block,
    },
    Config, SemanticallyVerifiedBlock,
};

/// Options for rolling back the finalized state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RollbackFinalizedStateOptions {
    /// Roll the finalized tip back to this height.
    pub target_height: block::Height,

    /// Write removed finalized blocks to the non-finalized backup cache.
    pub keep_rolled_back_blocks: bool,

    /// The maximum checkpoint height the node will use at its next startup.
    ///
    /// When `keep_rolled_back_blocks` is set, the kept blocks are only loaded back into the
    /// non-finalized state on the next start if the new finalized tip is at or above this height
    /// (see `StateService::new`'s `is_finalized_tip_past_max_checkpoint` gate). Rolling back below
    /// it would silently discard the backup, so rollback refuses that combination when this is
    /// `Some`. Callers that cannot determine the height (or want to skip the check) pass `None`.
    pub max_checkpoint_height: Option<block::Height>,
}

/// Details about non-finalized backup files written by rollback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RollbackBackupSummary {
    /// Backup directory used for rolled-back blocks.
    pub path: PathBuf,

    /// Number of rolled-back blocks written to the backup cache.
    pub block_count: usize,
}

/// Summary of a finalized-state rollback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RollbackFinalizedStateSummary {
    /// Finalized tip before rollback.
    pub old_tip: (block::Height, block::Hash),

    /// Finalized tip after rollback.
    pub new_tip: (block::Height, block::Hash),

    /// Number of finalized blocks removed.
    pub rolled_back_count: u32,

    /// Backup details when rolled-back blocks were kept.
    pub backup: Option<RollbackBackupSummary>,
}

/// Errors returned by finalized-state rollback.
#[derive(Debug, thiserror::Error)]
pub enum RollbackFinalizedStateError {
    /// The on-disk state database format does not match the running code.
    #[error(
        "state database format mismatch: on disk {on_disk:?}, running code {in_code}; \
         use a Zebra binary with the same state format"
    )]
    FormatMismatch {
        /// Version read from disk.
        on_disk: Option<Version>,
        /// Version implemented by the running code.
        in_code: Version,
    },

    /// The state database has no finalized tip.
    #[error("state database is empty")]
    EmptyState,

    /// The requested target height is above the finalized tip.
    #[error("target height {target:?} is above finalized tip {tip:?}")]
    TargetAboveTip {
        /// Requested rollback height.
        target: block::Height,
        /// Current finalized tip height.
        tip: block::Height,
    },

    /// The requested target height is the finalized tip.
    #[error("target height {target:?} is already the finalized tip")]
    TargetIsTip {
        /// Requested rollback height.
        target: block::Height,
    },

    /// The requested target height is missing from the finalized chain.
    #[error("target height {target:?} is missing from hash_by_height")]
    MissingTarget {
        /// Requested rollback height.
        target: block::Height,
    },

    /// Keeping rolled-back blocks is bounded by Zebra's non-finalized restore window.
    #[error(
        "cannot keep {count} rolled-back blocks: maximum non-finalized restore depth is {max}"
    )]
    KeepDepthTooLarge {
        /// Requested number of kept blocks.
        count: u32,
        /// Maximum supported number of kept blocks.
        max: u32,
    },

    /// Keeping rolled-back blocks below the max checkpoint height would silently discard them.
    #[error(
        "cannot keep rolled-back blocks: new finalized tip height {target:?} is below the max \
         checkpoint height {max_checkpoint:?}. The non-finalized restore only reloads blocks once \
         the finalized tip is past the last checkpoint, so the kept blocks would be deleted on the \
         next start. Roll back to a height at or above the max checkpoint, or omit \
         --keep-rolled-back-blocks."
    )]
    KeepBelowMaxCheckpoint {
        /// Requested new finalized tip height.
        target: block::Height,
        /// Max checkpoint height the node will use at its next startup.
        max_checkpoint: block::Height,
    },

    /// Non-finalized backup is disabled for this state configuration.
    #[error("cannot keep rolled-back blocks because non-finalized backup is disabled")]
    BackupDisabled,

    /// A block required for rollback could not be loaded.
    #[error("missing finalized block at height {height:?}")]
    MissingBlock {
        /// Missing block height.
        height: block::Height,
    },

    /// A transaction required for rollback could not be loaded.
    #[error("missing finalized transaction {hash:?}")]
    MissingTransaction {
        /// Missing transaction hash.
        hash: transaction::Hash,
    },

    /// A transparent output required for rollback could not be loaded.
    #[error("missing transparent output {outpoint:?}")]
    MissingTransparentOutput {
        /// Missing transparent outpoint.
        outpoint: transparent::OutPoint,
    },

    /// A transparent address balance required for rollback could not be loaded.
    #[error("missing transparent address balance for {address:?}")]
    MissingAddressBalance {
        /// Missing transparent address.
        address: transparent::Address,
    },

    /// A Sapling note commitment tree required for rollback could not be loaded.
    #[error("missing Sapling note commitment tree at height {height:?}")]
    MissingSaplingTree {
        /// Missing tree height.
        height: block::Height,
    },

    /// An Orchard note commitment tree required for rollback could not be loaded.
    #[error("missing Orchard note commitment tree at height {height:?}")]
    MissingOrchardTree {
        /// Missing tree height.
        height: block::Height,
    },

    /// Address balance arithmetic failed while reversing transparent indexes.
    #[error("transparent address balance update failed")]
    AddressBalance(#[from] amount::Error),

    /// Rebuilding note commitment trees failed.
    #[error("failed to rebuild note commitment trees")]
    NoteCommitmentTree(#[from] NoteCommitmentTreeError),

    /// Rebuilding the history tree failed.
    #[error("failed to rebuild history tree")]
    HistoryTree(#[from] HistoryTreeError),

    /// Computing a block subsidy failed.
    #[error("failed to compute block subsidy")]
    Subsidy(#[from] SubsidyError),

    /// Amount arithmetic failed while computing deferred pool balance changes.
    #[error("failed to compute deferred pool balance change")]
    DeferredPoolBalance(#[source] amount::Error),

    /// RocksDB failed while writing the rollback batch.
    #[error("failed to write rollback batch")]
    RocksDb(#[from] rocksdb::Error),

    /// Non-finalized backup file I/O failed.
    #[error("failed to update non-finalized backup cache")]
    BackupIo(#[from] std::io::Error),
}

/// Preview a finalized-state rollback without mutating the database.
pub fn preview_rollback_finalized_state(
    config: Config,
    network: &Network,
    options: RollbackFinalizedStateOptions,
) -> Result<RollbackFinalizedStateSummary, RollbackFinalizedStateError> {
    check_format_version(&config, network)?;

    let db = open_rollback_db(&config, network, true);
    let bounds = validate_rollback(&db, &options)?;

    // A dry run only reports the plan, so it deliberately skips the genesis-to-target treestate
    // rebuild and batch construction that `rollback_finalized_state` performs. The real run
    // recomputes those; doing them here would make the preview as slow as the rollback itself.
    Ok(bounds.summary(None))
}

/// Roll back the finalized state database to `options.target_height`.
///
/// The database is opened writable. If another Zebra process is running, RocksDB's
/// lock prevents this function from opening the state.
pub fn rollback_finalized_state(
    config: Config,
    network: &Network,
    options: RollbackFinalizedStateOptions,
) -> Result<RollbackFinalizedStateSummary, RollbackFinalizedStateError> {
    check_format_version(&config, network)?;

    let db = open_rollback_db(&config, network, false);
    let prepared = prepare_rollback(&db, network, &options)?;

    let backup = if options.keep_rolled_back_blocks {
        let backup_dir = config
            .non_finalized_state_backup_dir(network)
            .ok_or(RollbackFinalizedStateError::BackupDisabled)?;

        clear_backup_dir(&backup_dir)?;

        for block in prepared.removed_blocks.iter().rev() {
            write_semantically_verified_backup_block(&backup_dir, block)?;
        }

        Some(RollbackBackupSummary {
            path: backup_dir,
            block_count: prepared.removed_blocks.len(),
        })
    } else {
        if let Some(backup_dir) = config.non_finalized_state_backup_dir(network) {
            clear_backup_dir(&backup_dir)?;
        }

        None
    };

    let summary = prepared.summary(backup);

    db.write_batch(prepared.batch)?;

    Ok(summary)
}

fn check_format_version(
    config: &Config,
    network: &Network,
) -> Result<(), RollbackFinalizedStateError> {
    let in_code = state_database_format_version_in_code();
    let on_disk = state_database_format_version_on_disk(config, network).map_err(|_| {
        RollbackFinalizedStateError::FormatMismatch {
            on_disk: None,
            in_code: in_code.clone(),
        }
    })?;

    match on_disk {
        Some(on_disk) if on_disk == in_code => Ok(()),
        Some(on_disk) => Err(RollbackFinalizedStateError::FormatMismatch {
            on_disk: Some(on_disk),
            in_code,
        }),
        None => Err(RollbackFinalizedStateError::EmptyState),
    }
}

fn open_rollback_db(config: &Config, network: &Network, read_only: bool) -> ZebraDb {
    ZebraDb::new(
        config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        network,
        // Skip format upgrades: `check_format_version` already confirmed the on-disk format matches
        // the running code, so no upgrade is needed. Skipping also avoids spawning the background
        // format-change thread, which could otherwise mutate the database concurrently with the
        // rollback's own batch write.
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        read_only,
    )
}

/// The validated bounds of a rollback: where the finalized tip is now, and where it will end up.
///
/// Computing these is cheap (a couple of index lookups), so the dry-run preview stops here
/// instead of building the full rollback batch.
struct RollbackBounds {
    old_tip: (block::Height, block::Hash),
    new_tip: (block::Height, block::Hash),
}

impl RollbackBounds {
    fn summary(&self, backup: Option<RollbackBackupSummary>) -> RollbackFinalizedStateSummary {
        RollbackFinalizedStateSummary {
            old_tip: self.old_tip,
            new_tip: self.new_tip,
            rolled_back_count: self.old_tip.0 .0 - self.new_tip.0 .0,
            backup,
        }
    }
}

struct PreparedRollback {
    bounds: RollbackBounds,
    batch: DiskWriteBatch,
    removed_blocks: Vec<SemanticallyVerifiedBlock>,
}

impl PreparedRollback {
    fn summary(&self, backup: Option<RollbackBackupSummary>) -> RollbackFinalizedStateSummary {
        self.bounds.summary(backup)
    }
}

/// Validate a rollback request against the finalized state, without rebuilding any treestate or
/// building the rollback batch.
fn validate_rollback(
    db: &ZebraDb,
    options: &RollbackFinalizedStateOptions,
) -> Result<RollbackBounds, RollbackFinalizedStateError> {
    let old_tip = db.tip().ok_or(RollbackFinalizedStateError::EmptyState)?;
    let (old_tip_height, _) = old_tip;

    if options.target_height > old_tip_height {
        return Err(RollbackFinalizedStateError::TargetAboveTip {
            target: options.target_height,
            tip: old_tip_height,
        });
    }

    if options.target_height == old_tip_height {
        return Err(RollbackFinalizedStateError::TargetIsTip {
            target: options.target_height,
        });
    }

    let target_hash =
        db.hash(options.target_height)
            .ok_or(RollbackFinalizedStateError::MissingTarget {
                target: options.target_height,
            })?;

    if options.keep_rolled_back_blocks {
        let rollback_count = old_tip_height.0 - options.target_height.0;
        if rollback_count > crate::MAX_BLOCK_REORG_HEIGHT {
            return Err(RollbackFinalizedStateError::KeepDepthTooLarge {
                count: rollback_count,
                max: crate::MAX_BLOCK_REORG_HEIGHT,
            });
        }

        // The non-finalized restore only reloads the kept blocks once the finalized tip is past
        // the last checkpoint, so refuse rather than silently discarding them on the next start.
        if let Some(max_checkpoint) = options
            .max_checkpoint_height
            .filter(|&max| options.target_height < max)
        {
            return Err(RollbackFinalizedStateError::KeepBelowMaxCheckpoint {
                target: options.target_height,
                max_checkpoint,
            });
        }
    }

    Ok(RollbackBounds {
        old_tip,
        new_tip: (options.target_height, target_hash),
    })
}

fn prepare_rollback(
    db: &ZebraDb,
    network: &Network,
    options: &RollbackFinalizedStateOptions,
) -> Result<PreparedRollback, RollbackFinalizedStateError> {
    let bounds = validate_rollback(db, options)?;
    let (old_tip_height, _) = bounds.old_tip;

    let target_value_pool = db
        .block_info(options.target_height.into())
        .ok_or(RollbackFinalizedStateError::MissingTarget {
            target: options.target_height,
        })?
        .value_pools()
        .to_owned();

    let mut batch = DiskWriteBatch::new();
    let mut address_balances = HashMap::new();
    let mut removed_blocks = Vec::new();
    let mut removed_blocks_have_sprout_commitments = false;

    for height in ((options.target_height.0 + 1)..=old_tip_height.0)
        .rev()
        .map(Height)
    {
        let block = db
            .block(height.into())
            .ok_or(RollbackFinalizedStateError::MissingBlock { height })?;
        let semantically_verified = SemanticallyVerifiedBlock::from(block.clone())
            .with_deferred_pool_balance_change(deferred_pool_balance_change(height, network)?);

        reverse_transparent_block(
            db,
            network,
            &mut batch,
            &mut address_balances,
            height,
            &block,
        )?;
        delete_shielded_block(db, &mut batch, &block);
        delete_block_and_transaction_data(db, &mut batch, height, &block)?;

        removed_blocks_have_sprout_commitments |= block_has_sprout_commitments(&block);
        removed_blocks.push(semantically_verified);
    }

    let target_treestate = prepare_target_treestate(
        db,
        network,
        options.target_height,
        removed_blocks_have_sprout_commitments,
    )?;

    write_address_balances(db, &mut batch, address_balances);
    reset_tip_trees(db, &mut batch, &target_treestate);
    reset_value_pool(db, &mut batch, &target_value_pool);
    prune_tree_indexes(
        db,
        &mut batch,
        options.target_height,
        &target_treestate.retained_sprout_roots,
    );

    Ok(PreparedRollback {
        bounds,
        batch,
        removed_blocks,
    })
}

struct RebuiltTreestate {
    sprout_tree: Arc<zebra_chain::sprout::tree::NoteCommitmentTree>,
    history_tree: HistoryTree,
    retained_sprout_roots: Option<HashSet<zebra_chain::sprout::tree::Root>>,
}

fn prepare_target_treestate(
    db: &ZebraDb,
    network: &Network,
    target_height: Height,
    removed_blocks_have_sprout_commitments: bool,
) -> Result<RebuiltTreestate, RollbackFinalizedStateError> {
    if NetworkUpgrade::current(network, target_height) >= NetworkUpgrade::Canopy
        && !removed_blocks_have_sprout_commitments
    {
        return load_modern_treestate_at_height(db, network, target_height);
    }

    rebuild_treestate_to_height(db, network, target_height)
}

fn load_modern_treestate_at_height(
    db: &ZebraDb,
    network: &Network,
    target_height: Height,
) -> Result<RebuiltTreestate, RollbackFinalizedStateError> {
    let sprout_tree = db.sprout_tree_for_tip();
    let history_tree = rebuild_history_tree_from_upgrade_activation(db, network, target_height)?;

    Ok(RebuiltTreestate {
        sprout_tree,
        history_tree,
        // No removed block changed the Sprout note commitment tree, so the current tip tree is
        // exactly the target tree and no Sprout anchors above the target were created.
        retained_sprout_roots: None,
    })
}

fn block_has_sprout_commitments(block: &Block) -> bool {
    block.sprout_note_commitments().next().is_some()
}

fn rebuild_history_tree_from_upgrade_activation(
    db: &ZebraDb,
    network: &Network,
    target_height: Height,
) -> Result<HistoryTree, RollbackFinalizedStateError> {
    let network_upgrade = NetworkUpgrade::current(network, target_height);

    if network_upgrade < NetworkUpgrade::Heartwood {
        return Ok(HistoryTree::default());
    }

    let start_height = network_upgrade
        .activation_height(network)
        .expect("current network upgrade must have an activation height");

    let (block, sapling_root, orchard_root) = history_rebuild_inputs_at_height(db, start_height)?;
    let mut history_tree = HistoryTree::from_block(network, block, &sapling_root, &orchard_root)?;

    for height in ((start_height.0 + 1)..=target_height.0).map(Height) {
        let (block, sapling_root, orchard_root) = history_rebuild_inputs_at_height(db, height)?;

        history_tree.push(network, block, &sapling_root, &orchard_root)?;
    }

    Ok(history_tree)
}

fn history_rebuild_inputs_at_height(
    db: &ZebraDb,
    height: Height,
) -> Result<
    (
        Arc<Block>,
        zebra_chain::sapling::tree::Root,
        zebra_chain::orchard::tree::Root,
    ),
    RollbackFinalizedStateError,
> {
    let block = db
        .block(height.into())
        .ok_or(RollbackFinalizedStateError::MissingBlock { height })?;
    let sapling_root = db
        .sapling_tree_by_height(&height)
        .ok_or(RollbackFinalizedStateError::MissingSaplingTree { height })?
        .root();
    let orchard_root = db
        .orchard_tree_by_height(&height)
        .ok_or(RollbackFinalizedStateError::MissingOrchardTree { height })?
        .root();

    Ok((block, sapling_root, orchard_root))
}

fn rebuild_treestate_to_height(
    db: &ZebraDb,
    network: &Network,
    target_height: Height,
) -> Result<RebuiltTreestate, RollbackFinalizedStateError> {
    let mut note_commitment_trees = NoteCommitmentTrees::default();
    let mut history_tree = HistoryTree::default();
    let mut retained_sprout_roots = HashSet::new();

    for height in (Height::MIN.0..=target_height.0).map(Height) {
        let block = db
            .block(height.into())
            .ok_or(RollbackFinalizedStateError::MissingBlock { height })?;

        note_commitment_trees.update_trees_parallel(&block)?;
        retained_sprout_roots.insert(note_commitment_trees.sprout.root());

        let sapling_root = note_commitment_trees.sapling.root();
        let orchard_root = note_commitment_trees.orchard.root();
        history_tree.push(network, block, &sapling_root, &orchard_root)?;
    }

    Ok(RebuiltTreestate {
        sprout_tree: note_commitment_trees.sprout,
        history_tree,
        retained_sprout_roots: Some(retained_sprout_roots),
    })
}

fn deferred_pool_balance_change(
    height: Height,
    network: &Network,
) -> Result<Option<DeferredPoolBalanceChange>, RollbackFinalizedStateError> {
    if height <= network.slow_start_interval() {
        return Ok(None);
    }

    let deferred_amount = funding_stream_values(height, network, block_subsidy(height, network)?)?
        .remove(&FundingStreamReceiver::Deferred)
        .unwrap_or_default()
        .checked_sub(network.lockbox_disbursement_total_amount(height))
        .ok_or_else(|| {
            RollbackFinalizedStateError::DeferredPoolBalance(amount::Error::Constraint {
                value: i64::MIN,
                range: -zebra_chain::amount::MAX_MONEY..=zebra_chain::amount::MAX_MONEY,
            })
        })?;

    Ok(Some(DeferredPoolBalanceChange::new(deferred_amount)))
}

fn reverse_transparent_block(
    db: &ZebraDb,
    network: &Network,
    batch: &mut DiskWriteBatch,
    address_balances: &mut HashMap<transparent::Address, Option<AddressBalanceLocation>>,
    height: Height,
    block: &Arc<Block>,
) -> Result<(), RollbackFinalizedStateError> {
    // Undo the forward write transaction-by-transaction in reverse order, un-crediting each
    // transaction's created outputs before un-debiting its spent inputs.
    //
    // The forward write debits inputs before crediting outputs within each transaction so that
    // every intermediate per-address balance stays within the consensus range, even for a
    // same-address self-spend chain whose credit-first intermediate balance would exceed MAX_MONEY
    // (see `prepare_transparent_transaction_batch`). Undoing the operations in the exact reverse
    // order retraces those same in-range intermediate balances, so the checked balance arithmetic
    // below cannot spuriously overflow or underflow.
    for (tx_index, transaction) in block.transactions.iter().enumerate().rev() {
        let tx_location = TransactionLocation::from_usize(height, tx_index);

        // Un-credit the outputs this transaction created.
        for (output_index, output) in transaction.outputs().iter().enumerate() {
            let created_output_location =
                OutputLocation::from_usize(height, tx_index, output_index);

            if let Some(address) = output.address(network) {
                let address_location =
                    cached_address_balance(db, address_balances, &address)?.address_location();

                batch.zs_delete(
                    db.db.cf_handle("tx_loc_by_transparent_addr_loc").unwrap(),
                    AddressTransaction::new(address_location, tx_location),
                );
                batch.zs_delete(
                    db.db.cf_handle("utxo_loc_by_transparent_addr_loc").unwrap(),
                    AddressUnspentOutput::new(address_location, created_output_location),
                );

                sub_address_balance(db, address_balances, &address, output.value())?;
                sub_address_received(db, address_balances, &address, output.value());
            }

            batch.zs_delete(
                db.db.cf_handle("utxo_by_out_loc").unwrap(),
                created_output_location,
            );
        }

        // Un-debit the outputs this transaction spent.
        for spent_outpoint in transaction.inputs().iter().filter_map(Input::outpoint) {
            let (spent_output_location, spent_utxo) = finalized_output(db, &spent_outpoint)?;

            if let Some(address) = spent_utxo.output.address(network) {
                let address_location =
                    cached_address_balance(db, address_balances, &address)?.address_location();

                batch.zs_delete(
                    db.db.cf_handle("tx_loc_by_transparent_addr_loc").unwrap(),
                    AddressTransaction::new(address_location, tx_location),
                );
                batch.zs_insert(
                    db.db.cf_handle("utxo_loc_by_transparent_addr_loc").unwrap(),
                    AddressUnspentOutput::new(address_location, spent_output_location),
                    (),
                );

                add_address_balance(db, address_balances, &address, spent_utxo.output.value())?;
            }

            batch.zs_insert(
                db.db.cf_handle("utxo_by_out_loc").unwrap(),
                spent_output_location,
                &spent_utxo.output,
            );
            batch.zs_delete(
                db.db.cf_handle(TX_LOC_BY_SPENT_OUT_LOC).unwrap(),
                spent_output_location,
            );
        }
    }

    Ok(())
}

#[allow(clippy::unwrap_in_result)]
fn finalized_output(
    db: &ZebraDb,
    outpoint: &transparent::OutPoint,
) -> Result<(OutputLocation, transparent::Utxo), RollbackFinalizedStateError> {
    let transaction_location = db.transaction_location(outpoint.hash).ok_or(
        RollbackFinalizedStateError::MissingTransaction {
            hash: outpoint.hash,
        },
    )?;
    let transaction =
        db.transaction(outpoint.hash)
            .ok_or(RollbackFinalizedStateError::MissingTransaction {
                hash: outpoint.hash,
            })?;
    let output = transaction
        .0
        .outputs()
        .get(usize::try_from(outpoint.index).expect("valid output indexes fit in usize"))
        .ok_or(RollbackFinalizedStateError::MissingTransparentOutput {
            outpoint: *outpoint,
        })?
        .clone();
    let output_location = OutputLocation::from_outpoint(transaction_location, outpoint);

    Ok((
        output_location,
        transparent::Utxo::from_location(
            output,
            transaction_location.height,
            transaction_location.index.as_usize(),
        ),
    ))
}

fn cached_address_balance<'a>(
    db: &ZebraDb,
    address_balances: &'a mut HashMap<transparent::Address, Option<AddressBalanceLocation>>,
    address: &transparent::Address,
) -> Result<&'a mut AddressBalanceLocation, RollbackFinalizedStateError> {
    if !address_balances.contains_key(address) {
        address_balances.insert(*address, db.address_balance_location(address));
    }

    address_balances
        .get_mut(address)
        .and_then(Option::as_mut)
        .ok_or(RollbackFinalizedStateError::MissingAddressBalance { address: *address })
}

fn add_address_balance(
    db: &ZebraDb,
    address_balances: &mut HashMap<transparent::Address, Option<AddressBalanceLocation>>,
    address: &transparent::Address,
    value: Amount<NonNegative>,
) -> Result<(), RollbackFinalizedStateError> {
    let balance = cached_address_balance(db, address_balances, address)?;
    *balance.balance_mut() = (balance.balance() + value)?;
    Ok(())
}

fn sub_address_balance(
    db: &ZebraDb,
    address_balances: &mut HashMap<transparent::Address, Option<AddressBalanceLocation>>,
    address: &transparent::Address,
    value: Amount<NonNegative>,
) -> Result<(), RollbackFinalizedStateError> {
    let balance = cached_address_balance(db, address_balances, address)?;
    *balance.balance_mut() = (balance.balance() - value)?;
    Ok(())
}

fn sub_address_received(
    db: &ZebraDb,
    address_balances: &mut HashMap<transparent::Address, Option<AddressBalanceLocation>>,
    address: &transparent::Address,
    value: Amount<NonNegative>,
) {
    let balance = cached_address_balance(db, address_balances, address)
        .expect("address balance was loaded before subtracting received value");
    let value = u64::try_from(value.zatoshis()).expect("non-negative zatoshi amount fits in u64");
    *balance.received_mut() = balance.received().saturating_sub(value);
}

fn write_address_balances(
    db: &ZebraDb,
    batch: &mut DiskWriteBatch,
    address_balances: HashMap<transparent::Address, Option<AddressBalanceLocation>>,
) {
    let balance_cf = db.db.cf_handle(BALANCE_BY_TRANSPARENT_ADDR).unwrap();

    for (address, balance) in address_balances {
        match balance {
            Some(balance) if balance.balance().is_zero() && balance.received() == 0 => {
                batch.zs_delete(&balance_cf, address);
            }
            Some(balance) => batch.zs_insert(&balance_cf, address, balance),
            None => batch.zs_delete(&balance_cf, address),
        }
    }
}

fn delete_shielded_block(db: &ZebraDb, batch: &mut DiskWriteBatch, block: &Block) {
    let sprout_nullifiers = db.db.cf_handle("sprout_nullifiers").unwrap();
    let sapling_nullifiers = db.db.cf_handle("sapling_nullifiers").unwrap();
    let orchard_nullifiers = db.db.cf_handle("orchard_nullifiers").unwrap();

    for transaction in &block.transactions {
        for nullifier in transaction.sprout_nullifiers() {
            batch.zs_delete(&sprout_nullifiers, nullifier);
        }
        for nullifier in transaction.sapling_nullifiers() {
            batch.zs_delete(&sapling_nullifiers, nullifier);
        }
        for nullifier in transaction.orchard_nullifiers() {
            batch.zs_delete(&orchard_nullifiers, nullifier);
        }
    }
}

fn delete_block_and_transaction_data(
    db: &ZebraDb,
    batch: &mut DiskWriteBatch,
    height: Height,
    block: &Block,
) -> Result<(), RollbackFinalizedStateError> {
    let block_hash = db
        .hash(height)
        .ok_or(RollbackFinalizedStateError::MissingBlock { height })?;

    batch.zs_delete(db.db.cf_handle("block_header_by_height").unwrap(), height);
    batch.zs_delete(db.db.cf_handle("hash_by_height").unwrap(), height);
    batch.zs_delete(db.db.cf_handle("height_by_hash").unwrap(), block_hash);
    batch.zs_delete(db.db.cf_handle(BLOCK_INFO).unwrap(), height);

    for tx_index in 0..block.transactions.len() {
        let tx_location = TransactionLocation::from_usize(height, tx_index);
        let tx_hash = db
            .transaction_hash(tx_location)
            .ok_or(RollbackFinalizedStateError::MissingBlock { height })?;

        batch.zs_delete(db.db.cf_handle("tx_by_loc").unwrap(), tx_location);
        batch.zs_delete(db.db.cf_handle("hash_by_tx_loc").unwrap(), tx_location);
        batch.zs_delete(db.db.cf_handle("tx_loc_by_hash").unwrap(), tx_hash);
    }

    Ok(())
}

fn reset_tip_trees(db: &ZebraDb, batch: &mut DiskWriteBatch, treestate: &RebuiltTreestate) {
    // The sprout and history tip trees live in single-entry column families, so overwrite them
    // with the trees rebuilt up to the target height.
    batch.update_sprout_tree(db, &treestate.sprout_tree);
    batch.update_history_tree(db, &treestate.history_tree);

    // The sapling and orchard trees are height-keyed and de-duplicated: the forward write only
    // stores a tree when its root changes, and reads find the tip tree by searching backwards.
    // Deleting the trees above the target height (see `prune_tree_indexes`) therefore already
    // leaves the correct de-duplicated trees for the new tip. Writing a tree at the target height
    // here would instead create a duplicate entry whenever the target block added no notes, which
    // the de-duplicate-tree format check rejects on the next startup.
}

fn reset_value_pool(
    db: &ZebraDb,
    batch: &mut DiskWriteBatch,
    value_pool: &ValueBalance<NonNegative>,
) {
    let _ = db
        .chain_value_pools_cf()
        .with_batch_for_writing(batch)
        .zs_insert(&(), value_pool);
}

fn prune_tree_indexes(
    db: &ZebraDb,
    batch: &mut DiskWriteBatch,
    target_height: Height,
    retained_sprout_roots: &Option<HashSet<zebra_chain::sprout::tree::Root>>,
) {
    let sapling_trees: BTreeMap<_, _> = db
        .sapling_tree_by_height_range((
            std::ops::Bound::Excluded(target_height),
            std::ops::Bound::Unbounded,
        ))
        .collect();
    for (height, tree) in sapling_trees {
        batch.delete_sapling_tree(db, &height);
        batch.delete_sapling_anchor(db, &tree.root());
    }

    let orchard_trees: BTreeMap<_, _> = db
        .orchard_tree_by_height_range((
            std::ops::Bound::Excluded(target_height),
            std::ops::Bound::Unbounded,
        ))
        .collect();
    for (height, tree) in orchard_trees {
        batch.delete_orchard_tree(db, &height);
        batch.delete_orchard_anchor(db, &tree.root());
    }

    // Delete every sapling/orchard subtree whose notes extend past the target height. Subtree
    // indexes are read back from the database and number far fewer than `u16::MAX`, so `index.0 + 1`
    // (the exclusive end of the single-index delete range) cannot overflow.
    for (index, _) in db
        .sapling_subtree_list_by_index_range(..)
        .into_iter()
        .filter(|(_, subtree)| subtree.end_height > target_height)
    {
        batch.delete_range_sapling_subtree(db, index, NoteCommitmentSubtreeIndex(index.0 + 1));
    }

    for (index, _) in db
        .orchard_subtree_list_by_index_range(..)
        .into_iter()
        .filter(|(_, subtree)| subtree.end_height > target_height)
    {
        batch.delete_range_orchard_subtree(db, index, NoteCommitmentSubtreeIndex(index.0 + 1));
    }

    // Sprout has no by-height anchor index, so enumerate every anchor and drop the ones not seen
    // while rebuilding the tree up to the target. This loads each historical sprout tree, but the
    // sprout pool has been inactive since before Sapling, so the anchor set is small and fixed.
    if let Some(retained_sprout_roots) = retained_sprout_roots {
        for (root, _) in db.sprout_trees_full_map() {
            if !retained_sprout_roots.contains(&root) {
                batch.delete_sprout_anchor(db, &root);
            }
        }
    }

    let next_height = Height(target_height.0 + 1);
    batch.delete_range_history_tree(db, &next_height, &Height::MAX);
    batch.delete_range_sprout_tree(db, &next_height, &Height::MAX);
}

fn clear_backup_dir(path: &PathBuf) -> Result<(), std::io::Error> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    std::fs::create_dir_all(path)
}
