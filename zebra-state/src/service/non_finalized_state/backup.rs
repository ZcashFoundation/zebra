use std::{
    collections::{BTreeMap, HashMap},
    fs::DirEntry,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use hex::ToHex;
use zebra_chain::{
    amount::{Amount, DeferredPoolBalanceChange},
    block::{self, Block, Height},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};

use crate::{
    ContextuallyVerifiedBlock, IntoDisk, NonFinalizedState, SemanticallyVerifiedBlock,
    WatchReceiver, ZebraDb,
};

#[cfg(not(test))]
use crate::service::write::validate_and_commit_non_finalized;

/// The minimum duration that Zebra will wait between updates to the non-finalized state backup cache.
pub(crate) const MIN_DURATION_BETWEEN_BACKUP_UPDATES: Duration = Duration::from_secs(5);

/// Accepts an optional path to the non-finalized state backup directory and a handle to the database.
///
/// Looks for blocks above the finalized tip height in the backup directory (if a path was provided) and
/// attempts to commit them to the non-finalized state.
///
/// Returns the resulting non-finalized state.
pub(super) fn restore_backup(
    mut non_finalized_state: NonFinalizedState,
    backup_dir_path: &PathBuf,
    finalized_state: &ZebraDb,
) -> NonFinalizedState {
    let mut store: BTreeMap<Height, Vec<SemanticallyVerifiedBlock>> = BTreeMap::new();

    for block in read_non_finalized_blocks_from_backup(backup_dir_path, finalized_state) {
        store.entry(block.height).or_default().push(block);
    }

    for (height, blocks) in store {
        for block in blocks {
            #[cfg(test)]
            let commit_result = if non_finalized_state
                .any_chain_contains(&block.block.header.previous_block_hash)
            {
                non_finalized_state.commit_block(block, finalized_state)
            } else {
                non_finalized_state.commit_new_chain(block, finalized_state)
            };

            #[cfg(not(test))]
            let commit_result =
                validate_and_commit_non_finalized(finalized_state, &mut non_finalized_state, block);

            // Re-computes the block hash in case the hash from the filename is wrong.
            if let Err(commit_error) = commit_result {
                tracing::warn!(
                    ?commit_error,
                    ?height,
                    "failed to commit non-finalized block from backup directory"
                );
            }
        }
    }

    non_finalized_state
}

/// Updates the non-finalized state backup cache whenever the non-finalized state changes,
/// deleting any outdated backup files and writing any blocks that are in the non-finalized
/// state but missing in the backup cache.
pub(super) async fn run_backup_task(
    mut non_finalized_state_receiver: WatchReceiver<NonFinalizedState>,
    backup_dir_path: PathBuf,
) {
    let err = loop {
        let rate_limit = tokio::time::sleep(MIN_DURATION_BETWEEN_BACKUP_UPDATES);
        let backup_blocks: HashMap<block::Hash, PathBuf> = {
            let backup_dir_path = backup_dir_path.clone();
            tokio::task::spawn_blocking(move || list_backup_dir_entries(&backup_dir_path))
                .await
                .expect("failed to join blocking task when reading in backup task")
                .collect()
        };

        if let (Err(err), _) = tokio::join!(non_finalized_state_receiver.changed(), rate_limit) {
            break err;
        };

        let latest_non_finalized_state = non_finalized_state_receiver.cloned_watch_data();

        let backup_dir_path = backup_dir_path.clone();
        tokio::task::spawn_blocking(move || {
            write_backup(&backup_dir_path, &latest_non_finalized_state, backup_blocks);
        })
        .await
        .expect("failed to join blocking task when writing in backup task");
    };

    tracing::warn!(
        ?err,
        "got recv error waiting on non-finalized state change, is Zebra shutting down?"
    );

    // Perform one final backup write so that non-finalized blocks are not lost on shutdown.
    // The watch receiver retains the last value after the sender is dropped.
    let final_non_finalized_state = non_finalized_state_receiver.cloned_watch_data();
    let final_backup_dir_path = backup_dir_path.clone();
    let final_backup_blocks: HashMap<block::Hash, PathBuf> = {
        match tokio::task::spawn_blocking(move || list_backup_dir_entries(&backup_dir_path))
            .await
        {
            Ok(entries) => entries.collect(),
            Err(join_err) => {
                tracing::warn!(
                    ?join_err,
                    "failed to list backup dir during final shutdown write"
                );
                return;
            }
        }
    };

    if let Err(join_err) = tokio::task::spawn_blocking(move || {
        write_backup(
            &final_backup_dir_path,
            &final_non_finalized_state,
            final_backup_blocks,
        );
    })
    .await
    {
        tracing::warn!(
            ?join_err,
            "failed to join blocking task during final shutdown backup write"
        );
        return;
    }

    tracing::info!("completed final non-finalized state backup write on shutdown");
}

/// Diffs the backup files against the current non-finalized state, writes any missing blocks,
/// and deletes any outdated backup files.
fn write_backup(
    backup_dir_path: &Path,
    non_finalized_state: &NonFinalizedState,
    mut backup_blocks: HashMap<block::Hash, PathBuf>,
) {
    for block in non_finalized_state
        .chain_iter()
        .flat_map(|chain| chain.blocks.values())
        // Remove blocks from `backup_blocks` that are present in the non-finalized state
        .filter(|block| backup_blocks.remove(&block.hash).is_none())
    {
        // This loop will typically iterate only once, but may write multiple blocks if it misses
        // some non-finalized state changes while waiting for I/O ops.
        write_backup_block(backup_dir_path, block);
    }

    // Remove any backup blocks that are not present in the non-finalized state
    for (_, outdated_backup_block_path) in backup_blocks {
        if let Err(delete_error) = std::fs::remove_file(outdated_backup_block_path) {
            tracing::warn!(?delete_error, "failed to delete backup block file");
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NonFinalizedBlockBackup {
    block: Arc<Block>,
    deferred_pool_balance_change: Amount,
}

impl From<&ContextuallyVerifiedBlock> for NonFinalizedBlockBackup {
    fn from(cv_block: &ContextuallyVerifiedBlock) -> Self {
        Self {
            block: cv_block.block.clone(),
            deferred_pool_balance_change: cv_block.chain_value_pool_change.deferred_amount(),
        }
    }
}

impl NonFinalizedBlockBackup {
    /// Encodes a [`NonFinalizedBlockBackup`] as a vector of bytes.
    fn as_bytes(&self) -> Vec<u8> {
        let block_bytes = self
            .block
            .zcash_serialize_to_vec()
            .expect("verified block header version should be valid");

        let deferred_pool_balance_change_bytes =
            self.deferred_pool_balance_change.as_bytes().to_vec();

        [deferred_pool_balance_change_bytes, block_bytes].concat()
    }

    /// Constructs a new [`NonFinalizedBlockBackup`] from a vector of bytes.
    #[allow(clippy::unwrap_in_result)]
    fn from_bytes(bytes: Vec<u8>) -> Result<Self, io::Error> {
        let (deferred_pool_balance_change_bytes, block_bytes) = bytes
            .split_at_checked(size_of::<Amount>())
            .ok_or(io::Error::new(
                ErrorKind::InvalidInput,
                "input is too short",
            ))?;

        Ok(Self {
            block: Arc::new(
                block_bytes
                    .zcash_deserialize_into()
                    .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?,
            ),
            deferred_pool_balance_change: Amount::from_bytes(
                deferred_pool_balance_change_bytes
                    .try_into()
                    .expect("slice from `split_at_checked()` should fit in [u8; 8]"),
            )
            .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?,
        })
    }
}

/// Writes a block to a file in the provided non-finalized state backup cache directory path.
fn write_backup_block(backup_dir_path: &Path, block: &ContextuallyVerifiedBlock) {
    let backup_block_file_name: String = block.hash.encode_hex();
    let backup_block_file_path = backup_dir_path.join(backup_block_file_name);
    let non_finalized_block_backup: NonFinalizedBlockBackup = block.into();

    if let Err(err) = std::fs::write(
        backup_block_file_path,
        non_finalized_block_backup.as_bytes(),
    ) {
        tracing::warn!(?err, "failed to write non-finalized state backup block");
    }
}

/// Reads blocks from the provided non-finalized state backup directory path.
///
/// Returns any blocks that are valid and not present in the finalized state.
fn read_non_finalized_blocks_from_backup<'a>(
    backup_dir_path: &PathBuf,
    finalized_state: &'a ZebraDb,
) -> impl Iterator<Item = SemanticallyVerifiedBlock> + 'a {
    list_backup_dir_entries(backup_dir_path)
        // It's okay to leave the file here, the backup task will delete it as long as
        // the block is not added to the non-finalized state.
        .filter(|&(block_hash, _)| !finalized_state.contains_hash(block_hash))
        .filter_map(|(block_hash, file_path)| match std::fs::read(file_path) {
            Ok(block_bytes) => Some((block_hash, block_bytes)),
            Err(err) => {
                tracing::warn!(?err, "failed to open non-finalized state backup block file");
                None
            }
        })
        .filter_map(|(expected_block_hash, backup_block_file_contents)| {
            match NonFinalizedBlockBackup::from_bytes(backup_block_file_contents) {
                Ok(NonFinalizedBlockBackup {
                    block,
                    deferred_pool_balance_change,
                }) if block.coinbase_height().is_some() => {
                    let block = SemanticallyVerifiedBlock::from(block)
                        .with_deferred_pool_balance_change(Some(DeferredPoolBalanceChange::new(
                            deferred_pool_balance_change,
                        )));
                    if block.hash != expected_block_hash {
                        tracing::warn!(
                            block_hash = ?block.hash,
                            ?expected_block_hash,
                            "wrong block hash in file name"
                        );
                    }
                    Some(block)
                }
                Ok(block) => {
                    tracing::warn!(
                        ?block,
                        "invalid non-finalized backup block, missing coinbase height"
                    );
                    None
                }
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        "failed to deserialize non-finalized backup data into block"
                    );
                    None
                }
            }
        })
}

/// Accepts a backup directory path, opens the directory, converts its entries
/// filenames to block hashes, and deletes any entries with invalid file names.
///
/// # Panics
///
/// If the provided path cannot be opened as a directory.
/// See [`read_backup_dir`] for more details.
fn list_backup_dir_entries(
    backup_dir_path: &PathBuf,
) -> impl Iterator<Item = (block::Hash, PathBuf)> {
    read_backup_dir(backup_dir_path).filter_map(process_backup_dir_entry)
}

/// Accepts a backup directory path and opens the directory.
///
/// Returns an iterator over all [`DirEntry`]s in the directory that are successfully read.
///
/// # Panics
///
/// If the provided path cannot be opened as a directory.
fn read_backup_dir(backup_dir_path: &PathBuf) -> impl Iterator<Item = DirEntry> {
    std::fs::read_dir(backup_dir_path)
        .expect("failed to read non-finalized state backup directory")
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(io_err) => {
                tracing::warn!(
                    ?io_err,
                    "failed to read DirEntry in non-finalized state backup dir"
                );

                None
            }
        })
}

/// Accepts a [`DirEntry`] from the non-finalized state backup directory and
/// parses the filename into a block hash.
///
/// Returns the block hash and the file path if successful, or
/// returns None and deletes the file at the entry path otherwise.
fn process_backup_dir_entry(entry: DirEntry) -> Option<(block::Hash, PathBuf)> {
    let delete_file = || {
        if let Err(delete_error) = std::fs::remove_file(entry.path()) {
            tracing::warn!(?delete_error, "failed to delete backup block file");
        }
    };

    let block_file_name = match entry.file_name().into_string() {
        Ok(block_hash) => block_hash,
        Err(err) => {
            tracing::warn!(
                ?err,
                "failed to convert OsString to String, attempting to delete file"
            );

            delete_file();
            return None;
        }
    };

    let block_hash: block::Hash = match block_file_name.parse() {
        Ok(block_hash) => block_hash,
        Err(err) => {
            tracing::warn!(
                ?err,
                "failed to parse hex-encoded block hash from file name, attempting to delete file"
            );

            delete_file();
            return None;
        }
    };

    Some((block_hash, entry.path()))
}
