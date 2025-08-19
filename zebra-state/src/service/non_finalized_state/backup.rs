use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use zebra_chain::{
    block::{self, Block, Height},
    serialization::ZcashDeserializeInto,
};

use crate::{service::write::validate_and_commit_non_finalized, NonFinalizedState, ZebraDb};

/// Accepts an optional path to the non-finalized state backup directory and a handle to the database.
///
/// Creates a new backup directory at the provided path if none exists.
///
/// Looks for blocks above the finalized tip height in the backup directory (if a path was provided) and
/// attempts to commit them to the non-finalized state.
///
/// Returns the resulting non-finalized state.
pub fn restore_backup(
    mut non_finalized_state: NonFinalizedState,
    backup_dir_path: PathBuf,
    finalized_state: &ZebraDb,
) -> NonFinalizedState {
    // Create a new backup directory if none exists
    std::fs::create_dir_all(&backup_dir_path)
        .expect("failed to create non-finalized state backup directory");

    let backup_dir = std::fs::read_dir(backup_dir_path)
        .expect("failed to read non-finalized state backup directory");

    let mut store: BTreeMap<Height, Vec<Arc<Block>>> = BTreeMap::new();

    for entry in backup_dir {
        let block_file_entry = match entry {
            Ok(entry) => entry,
            Err(io_err) => {
                tracing::warn!(
                    ?io_err,
                    "failed to read DirEntry in non-finalized state backup dir"
                );

                continue;
            }
        };

        let block_file_name = match block_file_entry.file_name().into_string() {
            Ok(block_hash) => block_hash,
            Err(err) => {
                tracing::warn!(?err, "failed to convert OsString to String");

                continue;
            }
        };

        let block_hash: block::Hash = match block_file_name.parse() {
            Ok(block_hash) => block_hash,
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "failed to parse hex-encoded block hash from file name"
                );

                continue;
            }
        };

        if finalized_state.contains_hash(block_hash) {
            // It's okay to leave the file here, the backup task will delete it as long as
            // the block is not added to the non-finalized state.
            tracing::info!("found finalized block in non-finalized state backup dir, ignoring");
            continue;
        }

        let block_data = match std::fs::read(&block_file_entry.path()) {
            Ok(block_data) => block_data,
            Err(err) => {
                tracing::warn!(?err, "failed to open non-finalized state backup block file");
                continue;
            }
        };

        let block: Arc<Block> = match block_data.zcash_deserialize_into() {
            Ok(block) => Arc::new(block),
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "failed to deserialize non-finalized backup data into block"
                );
                continue;
            }
        };

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
