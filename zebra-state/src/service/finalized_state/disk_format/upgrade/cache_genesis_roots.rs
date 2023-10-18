//! Updating the genesis note commitment trees to cache their roots.
//!
//! This reduces CPU usage when the genesis tree roots are used for transaction validation.
//! Since mempool transactions are cheap to create, this is a potential remote denial of service.

use std::sync::mpsc;

use zebra_chain::{block::Height, sprout};

use crate::service::finalized_state::{disk_db::DiskWriteBatch, ZebraDb};

use super::CancelFormatChange;

/// Runs disk format upgrade for changing the sprout and history tree key types.
///
/// Returns `Ok` if the upgrade completed, and `Err` if it was cancelled.
///
/// # Panics
///
/// If the state is empty.
#[allow(clippy::unwrap_in_result)]
#[instrument(skip(upgrade_db, cancel_receiver))]
pub fn run(
    _initial_tip_height: Height,
    upgrade_db: &ZebraDb,
    cancel_receiver: &mpsc::Receiver<CancelFormatChange>,
) -> Result<(), CancelFormatChange> {
    let sprout_genesis_tree = sprout::tree::NoteCommitmentTree::default();
    let sprout_tip_tree = upgrade_db.sprout_tree_for_tip();

    let sapling_genesis_tree = upgrade_db
        .sapling_tree_by_height(&Height(0))
        .expect("caller has checked for genesis block");
    let orchard_genesis_tree = upgrade_db
        .orchard_tree_by_height(&Height(0))
        .expect("caller has checked for genesis block");

    // Writing the trees back to the database automatically caches their roots.
    let mut batch = DiskWriteBatch::new();

    // Fix the cached root of the Sprout genesis tree in its anchors column family.

    // It's ok to write the genesis tree to the tip tree index, because it's overwritten by
    // the actual tip before the batch is written to the database.
    batch.update_sprout_tree(upgrade_db, &sprout_genesis_tree);
    // This method makes sure the sprout tip tree has a cached root, even if it's the genesis tree.
    batch.update_sprout_tree(upgrade_db, &sprout_tip_tree);

    batch.create_sapling_tree(upgrade_db, &Height(0), &sapling_genesis_tree);
    batch.create_orchard_tree(upgrade_db, &Height(0), &orchard_genesis_tree);

    // Return before we write if the upgrade is cancelled.
    if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
        return Err(CancelFormatChange);
    }

    upgrade_db
        .write_batch(batch)
        .expect("updating tree cached roots should always succeed");

    Ok(())
}

/// Quickly check that the genesis trees and sprout tip tree have cached roots.
///
/// This allows us to fail the upgrade quickly in tests and during development,
/// rather than waiting to see if it failed.
///
/// # Panics
///
/// If the state is empty.
pub fn quick_check(db: &ZebraDb) -> Result<(), String> {
    // An empty database doesn't have any trees, so its format is trivially correct.
    if db.is_empty() {
        return Ok(());
    }

    let sprout_genesis_tree = sprout::tree::NoteCommitmentTree::default();
    let sprout_genesis_tree = db
        .sprout_tree_by_anchor(&sprout_genesis_tree.root())
        .expect("just checked for genesis block");
    let sprout_tip_tree = db.sprout_tree_for_tip();

    let sapling_genesis_tree = db
        .sapling_tree_by_height(&Height(0))
        .expect("just checked for genesis block");
    let orchard_genesis_tree = db
        .orchard_tree_by_height(&Height(0))
        .expect("just checked for genesis block");

    // Check the entire format before returning any errors.
    let sprout_result = sprout_genesis_tree
        .cached_root()
        .ok_or("no cached root in sprout genesis tree");
    let sprout_tip_result = sprout_tip_tree
        .cached_root()
        .ok_or("no cached root in sprout tip tree");

    let sapling_result = sapling_genesis_tree
        .cached_root()
        .ok_or("no cached root in sapling genesis tree");
    let orchard_result = orchard_genesis_tree
        .cached_root()
        .ok_or("no cached root in orchard genesis tree");

    if sprout_result.is_err()
        || sprout_tip_result.is_err()
        || sapling_result.is_err()
        || orchard_result.is_err()
    {
        let err = Err(format!(
            "missing cached genesis root: sprout: {sprout_result:?}, {sprout_tip_result:?} \
             sapling: {sapling_result:?}, orchard: {orchard_result:?}"
        ));
        warn!(?err);
        return err;
    }

    Ok(())
}

/// Detailed check that all trees have cached roots.
///
/// # Panics
///
/// If the state is empty.
pub fn detailed_check(
    db: &ZebraDb,
    cancel_receiver: &mpsc::Receiver<CancelFormatChange>,
) -> Result<Result<(), String>, CancelFormatChange> {
    // This is redundant in some code paths, but not in others. But it's quick anyway.
    // Check the entire format before returning any errors.
    let mut result = quick_check(db);

    for (root, tree) in db.sprout_trees_full_map() {
        // Return early if the format check is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        if tree.cached_root().is_none() {
            result = Err(format!(
                "found un-cached sprout tree root after running genesis tree root fix \
                 {root:?}"
            ));
            error!(?result);
        }
    }

    for (height, tree) in db.sapling_tree_by_height_range(..) {
        // Return early if the format check is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        if tree.cached_root().is_none() {
            result = Err(format!(
                "found un-cached sapling tree root after running genesis tree root fix \
                 {height:?}"
            ));
            error!(?result);
        }
    }

    for (height, tree) in db.orchard_tree_by_height_range(..) {
        // Return early if the format check is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        if tree.cached_root().is_none() {
            result = Err(format!(
                "found un-cached orchard tree root after running genesis tree root fix \
                 {height:?}"
            ));
            error!(?result);
        }
    }

    Ok(result)
}
