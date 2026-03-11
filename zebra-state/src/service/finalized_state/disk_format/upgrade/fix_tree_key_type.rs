//! Updating the sprout and history tree key type from `Height` to the empty key `()`.
//!
//! This avoids a potential concurrency bug, and a known database performance issue.

use std::sync::Arc;

use crossbeam_channel::{Receiver, TryRecvError};
use zebra_chain::{block::Height, history_tree::HistoryTree, sprout};

use crate::service::finalized_state::{
    disk_db::DiskWriteBatch, disk_format::MAX_ON_DISK_HEIGHT, ZebraDb,
};

use super::CancelFormatChange;

/// Runs disk format upgrade for changing the sprout and history tree key types.
///
/// Returns `Ok` if the upgrade completed, and `Err` if it was cancelled.
#[allow(clippy::unwrap_in_result)]
#[instrument(skip(upgrade_db, cancel_receiver))]
pub fn run(
    _initial_tip_height: Height,
    upgrade_db: &ZebraDb,
    cancel_receiver: &Receiver<CancelFormatChange>,
) -> Result<(), CancelFormatChange> {
    let sprout_tip_tree = upgrade_db.sprout_tree_for_tip();
    let history_tip_tree = upgrade_db.history_tree();

    // Writing the trees back to the database automatically updates their format.
    let mut batch = DiskWriteBatch::new();

    // Update the sprout tip key format in the database.
    batch.update_sprout_tree(upgrade_db, &sprout_tip_tree);
    batch.update_history_tree(upgrade_db, &history_tip_tree);

    // Return before we write if the upgrade is cancelled.
    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
        return Err(CancelFormatChange);
    }

    upgrade_db
        .write_batch(batch)
        .expect("updating tree key formats should always succeed");

    // The deletes below can be slow due to tombstones for previously deleted keys,
    // so we do it in a separate batch to avoid data races with syncing (#7961).
    let mut batch = DiskWriteBatch::new();

    // Delete the previous `Height` tip key format, which is now a duplicate.
    // This doesn't delete the new `()` key format, because it serializes to an empty array.
    batch.delete_range_sprout_tree(upgrade_db, &Height(0), &MAX_ON_DISK_HEIGHT);
    batch.delete_range_history_tree(upgrade_db, &Height(0), &MAX_ON_DISK_HEIGHT);

    // Return before we write if the upgrade is cancelled.
    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
        return Err(CancelFormatChange);
    }

    upgrade_db
        .write_batch(batch)
        .expect("cleaning up old tree key formats should always succeed");

    Ok(())
}

/// Quickly check that the sprout and history tip trees have updated key formats.
///
/// # Panics
///
/// If the state is empty.
pub fn quick_check(db: &ZebraDb) -> Result<(), String> {
    // Check the entire format before returning any errors.
    let mut result = Ok(());

    let mut prev_key = None;
    let mut prev_tree: Option<Arc<sprout::tree::NoteCommitmentTree>> = None;

    for (key, tree) in db.sprout_trees_full_tip() {
        // The tip tree should be indexed by `()` (which serializes to an empty array).
        if !key.raw_bytes().is_empty() {
            result = Err(format!(
                "found incorrect sprout tree key format after running key format upgrade \
                 key: {key:?}, tree: {:?}",
                tree.root()
            ));
            error!(?result);
        }

        // There should only be one tip tree in this column family.
        if let Some(prev_tree) = prev_tree {
            result = Err(format!(
                "found duplicate sprout trees after running key format upgrade\n\
                 key: {key:?}, tree: {:?}\n\
                 prev key: {prev_key:?}, prev_tree: {:?}\n\
                 ",
                tree.root(),
                prev_tree.root(),
            ));
            error!(?result);
        }

        prev_key = Some(key);
        prev_tree = Some(tree);
    }

    let mut prev_key = None;
    let mut prev_tree: Option<Arc<HistoryTree>> = None;

    for (key, tree) in db.history_trees_full_tip() {
        // The tip tree should be indexed by `()` (which serializes to an empty array).
        if !key.raw_bytes().is_empty() {
            result = Err(format!(
                "found incorrect history tree key format after running key format upgrade \
                 key: {key:?}, tree: {:?}",
                tree.hash()
            ));
            error!(?result);
        }

        // There should only be one tip tree in this column family.
        if let Some(prev_tree) = prev_tree {
            result = Err(format!(
                "found duplicate history trees after running key format upgrade\n\
                 key: {key:?}, tree: {:?}\n\
                 prev key: {prev_key:?}, prev_tree: {:?}\n\
                 ",
                tree.hash(),
                prev_tree.hash(),
            ));
            error!(?result);
        }

        prev_key = Some(key);
        prev_tree = Some(tree);
    }

    result
}

/// Detailed check that the sprout and history tip trees have updated key formats.
/// This is currently the same as the quick check.
///
/// # Panics
///
/// If the state is empty.
pub fn detailed_check(
    db: &ZebraDb,
    _cancel_receiver: &Receiver<CancelFormatChange>,
) -> Result<Result<(), String>, CancelFormatChange> {
    // This upgrade only changes two key-value pairs, so checking it is always quick.
    Ok(quick_check(db))
}
