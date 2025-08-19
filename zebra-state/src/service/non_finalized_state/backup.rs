use std::{collections::BTreeMap, fs::DirEntry, path::PathBuf, sync::Arc};

use zebra_chain::{
    block::{self, Block, Height},
    serialization::ZcashDeserializeInto,
};

use crate::{
    service::write::validate_and_commit_non_finalized, NonFinalizedState, WatchReceiver, ZebraDb,
};

/// Accepts an optional path to the non-finalized state backup directory and a handle to the database.
///
/// Looks for blocks above the finalized tip height in the backup directory (if a path was provided) and
/// attempts to commit them to the non-finalized state.
///
/// Returns the resulting non-finalized state.
pub fn restore_backup(
    mut non_finalized_state: NonFinalizedState,
    backup_dir_path: &PathBuf,
    finalized_state: &ZebraDb,
) -> NonFinalizedState {
    let mut store: BTreeMap<Height, Vec<Arc<Block>>> = BTreeMap::new();

    for block in read_non_finalized_blocks_from_backup(backup_dir_path, finalized_state) {
        let Some(block_height) = block.coinbase_height() else {
            tracing::warn!("invalid non-finalized backup block, missing coinbase height");
            continue;
        };

        store.entry(block_height).or_default().push(block);
    }

    for (height, blocks) in store {
        for block in blocks {
            // Re-computes the block hash in case the hash from the filename is wrong.
            if let Err(commit_error) = validate_and_commit_non_finalized(
                finalized_state,
                &mut non_finalized_state,
                block.into(),
            ) {
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

// TODO: Spawn a task that deletes any files that aren't in the non-finalized state and writes files for
//       any blocks in the non-finalized state that are missing in the filesystem.
pub fn spawn_backup_task(
    _non_finalized_state_receiver: WatchReceiver<NonFinalizedState>,
    _backup_dir_path: PathBuf,
    _finalized_state: &ZebraDb,
) {
}

fn read_non_finalized_blocks_from_backup<'a>(
    backup_dir_path: &PathBuf,
    finalized_state: &'a ZebraDb,
) -> impl Iterator<Item = Arc<Block>> + 'a {
    list_backup_dir_entries(backup_dir_path)
        .into_iter()
        // It's okay to leave the file here, the backup task will delete it as long as
        // the block is not added to the non-finalized state.
        .filter(|&(block_hash, _)| finalized_state.contains_hash(block_hash))
        .filter_map(|(_, file_path)| match std::fs::read(file_path) {
            Ok(block_data) => Some(block_data),
            Err(err) => {
                tracing::warn!(?err, "failed to open non-finalized state backup block file");
                None
            }
        })
        .filter_map(|block_bytes| match block_bytes.zcash_deserialize_into() {
            Ok(block) => Some(Arc::new(block)),
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "failed to deserialize non-finalized backup data into block"
                );
                None
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
    read_backup_dir(backup_dir_path)
        .into_iter()
        .filter_map(process_backup_dir_entry)
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
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(io_err) => {
                tracing::warn!(
                    ?io_err,
                    "failed to read DirEntry in non-finalized state backup dir"
                );

                return None;
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
