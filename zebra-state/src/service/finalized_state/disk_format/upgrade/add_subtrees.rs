//! Fully populate the Sapling and Orchard note commitment subtrees for existing blocks in the database.

use std::sync::Arc;

use crossbeam_channel::{Receiver, TryRecvError};
use hex_literal::hex;
use itertools::Itertools;
use semver::Version;
use tracing::instrument;

use zebra_chain::{
    block::Height,
    orchard,
    parallel::tree::NoteCommitmentTrees,
    parameters::Network::*,
    sapling,
    subtree::{NoteCommitmentSubtree, NoteCommitmentSubtreeIndex},
};

use crate::service::finalized_state::{
    disk_format::upgrade::{CancelFormatChange, DiskFormatUpgrade},
    DiskWriteBatch, ZebraDb,
};

/// Implements [`DiskFormatUpgrade`] for populating Sapling and Orchard note commitment subtrees.
pub struct AddSubtrees;

impl DiskFormatUpgrade for AddSubtrees {
    fn version(&self) -> Version {
        Version::new(25, 2, 2)
    }

    fn description(&self) -> &'static str {
        "add subtrees upgrade"
    }

    fn prepare(
        &self,
        initial_tip_height: Height,
        upgrade_db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
        older_disk_version: &Version,
    ) -> Result<(), CancelFormatChange> {
        let first_version_for_adding_subtrees = Version::new(25, 2, 0);
        if older_disk_version >= &first_version_for_adding_subtrees {
            // Clear previous upgrade data, because it was incorrect.
            reset(initial_tip_height, upgrade_db, cancel_receiver)?;
        }

        Ok(())
    }

    /// Runs disk format upgrade for adding Sapling and Orchard note commitment subtrees to database.
    ///
    /// Trees are added to the database in reverse height order, so that wallets can sync correctly
    /// while the upgrade is running.
    ///
    /// Returns `Ok` if the upgrade completed, and `Err` if it was cancelled.
    fn run(
        &self,
        initial_tip_height: Height,
        upgrade_db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        // # Consensus
        //
        // Zebra stores exactly one note commitment tree for every block with sapling notes.
        // (It also stores the empty note commitment tree for the genesis block, but we skip that.)
        //
        // The consensus rules limit blocks to less than 2^16 sapling and 2^16 orchard outputs. So a
        // block can't complete multiple level 16 subtrees (or complete an entire subtree by itself).
        // Currently, with 2MB blocks and v4/v5 sapling and orchard output sizes, the subtree index can
        // increase by at most 1 every ~20 blocks.
        //
        // # Compatibility
        //
        // Because wallets search backwards from the chain tip, subtrees need to be added to the
        // database in reverse height order. (Tip first, genesis last.)
        //
        // Otherwise, wallets that sync during the upgrade will be missing some notes.

        // Generate a list of sapling subtree inputs: previous and current trees, and their end heights.
        let subtrees = upgrade_db
            .sapling_tree_by_reversed_height_range(..=initial_tip_height)
            // We need both the tree and its previous tree for each shielded block.
            .tuple_windows()
            // Because the iterator is reversed, the larger tree is first.
            .map(|((end_height, tree), (prev_end_height, prev_tree))| {
                (prev_end_height, prev_tree, end_height, tree)
            })
            // Find new subtrees.
            .filter(|(_prev_end_height, prev_tree, _end_height, tree)| {
                tree.contains_new_subtree(prev_tree)
            });

        for (prev_end_height, prev_tree, end_height, tree) in subtrees {
            // Return early if the upgrade is cancelled.
            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            let subtree =
                calculate_sapling_subtree(upgrade_db, prev_end_height, prev_tree, end_height, tree);
            write_sapling_subtree(upgrade_db, subtree);
        }

        // Generate a list of orchard subtree inputs: previous and current trees, and their end heights.
        let subtrees = upgrade_db
            .orchard_tree_by_reversed_height_range(..=initial_tip_height)
            // We need both the tree and its previous tree for each shielded block.
            .tuple_windows()
            // Because the iterator is reversed, the larger tree is first.
            .map(|((end_height, tree), (prev_end_height, prev_tree))| {
                (prev_end_height, prev_tree, end_height, tree)
            })
            // Find new subtrees.
            .filter(|(_prev_end_height, prev_tree, _end_height, tree)| {
                tree.contains_new_subtree(prev_tree)
            });

        for (prev_end_height, prev_tree, end_height, tree) in subtrees {
            // Return early if the upgrade is cancelled.
            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            let subtree =
                calculate_orchard_subtree(upgrade_db, prev_end_height, prev_tree, end_height, tree);
            write_orchard_subtree(upgrade_db, subtree);
        }

        Ok(())
    }

    #[allow(clippy::unwrap_in_result)]
    fn validate(
        &self,
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<Result<(), String>, CancelFormatChange> {
        // This is redundant in some code paths, but not in others. But it's quick anyway.
        let quick_result = subtree_format_calculation_pre_checks(db);

        // Check the entire format before returning any errors.
        let sapling_result = check_sapling_subtrees(db, cancel_receiver)?;
        let orchard_result = check_orchard_subtrees(db, cancel_receiver)?;

        if quick_result.is_err() || sapling_result.is_err() || orchard_result.is_err() {
            let err = Err(format!(
                "missing or invalid subtree(s): \
             quick: {quick_result:?}, sapling: {sapling_result:?}, orchard: {orchard_result:?}"
            ));
            warn!(?err);
            return Ok(err);
        }

        Ok(Ok(()))
    }
}

/// Reset data from previous upgrades. This data can be complete or incomplete.
///
/// Returns `Ok` if the upgrade completed, and `Err` if it was cancelled.
#[allow(clippy::unwrap_in_result)]
#[instrument(skip(upgrade_db, cancel_receiver))]
pub fn reset(
    _initial_tip_height: Height,
    upgrade_db: &ZebraDb,
    cancel_receiver: &Receiver<CancelFormatChange>,
) -> Result<(), CancelFormatChange> {
    // Return early if the upgrade is cancelled.
    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
        return Err(CancelFormatChange);
    }

    // This doesn't delete the maximum index, but the consensus rules make that subtree impossible.
    // (Adding a note to a full note commitment tree is an error.)
    //
    // TODO: convert zs_delete_range() to take std::ops::RangeBounds, and delete the upper bound.
    let mut batch = DiskWriteBatch::new();
    batch.delete_range_sapling_subtree(upgrade_db, 0.into(), u16::MAX.into());
    upgrade_db
        .write_batch(batch)
        .expect("deleting old sapling note commitment subtrees is a valid database operation");

    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
        return Err(CancelFormatChange);
    }

    let mut batch = DiskWriteBatch::new();
    batch.delete_range_orchard_subtree(upgrade_db, 0.into(), u16::MAX.into());
    upgrade_db
        .write_batch(batch)
        .expect("deleting old orchard note commitment subtrees is a valid database operation");

    Ok(())
}

/// Quickly check that the first calculated subtree is correct.
///
/// This allows us to fail the upgrade quickly in tests and during development,
/// rather than waiting ~20 minutes to see if it failed.
///
/// This check runs the first subtree calculation, but it doesn't read the subtree data in the
/// database. So it can be run before the upgrade is started.
pub fn subtree_format_calculation_pre_checks(db: &ZebraDb) -> Result<(), String> {
    // Check the entire format before returning any errors.
    let sapling_result = quick_check_sapling_subtrees(db);
    let orchard_result = quick_check_orchard_subtrees(db);

    if sapling_result.is_err() || orchard_result.is_err() {
        let err = Err(format!(
            "missing or bad first subtree: sapling: {sapling_result:?}, orchard: {orchard_result:?}"
        ));
        warn!(?err);
        return err;
    }

    Ok(())
}

/// A quick test vector that allows us to fail an incorrect upgrade within a few seconds.
fn first_sapling_mainnet_subtree() -> NoteCommitmentSubtree<sapling_crypto::Node> {
    // This test vector was generated using the command:
    // ```sh
    // zcash-cli z_getsubtreesbyindex sapling 0 1
    // ```
    NoteCommitmentSubtree {
        index: 0.into(),
        root: sapling_crypto::Node::from_bytes(hex!(
            "754bb593ea42d231a7ddf367640f09bbf59dc00f2c1d2003cc340e0c016b5b13"
        ))
        .expect("test vector is valid"),
        end_height: Height(558822),
    }
}

/// A quick test vector that allows us to fail an incorrect upgrade within a few seconds.
fn first_orchard_mainnet_subtree() -> NoteCommitmentSubtree<orchard::tree::Node> {
    // This test vector was generated using the command:
    // ```sh
    // zcash-cli z_getsubtreesbyindex orchard 0 1
    // ```
    NoteCommitmentSubtree {
        index: 0.into(),
        root: hex!("d4e323b3ae0cabfb6be4087fec8c66d9a9bbfc354bf1d9588b6620448182063b")
            .as_slice()
            .try_into()
            .expect("test vector is valid"),
        end_height: Height(1707429),
    }
}

/// Quickly check that the first calculated sapling subtree is correct.
///
/// This allows us to fail the upgrade quickly in tests and during development,
/// rather than waiting ~20 minutes to see if it failed.
///
/// Returns an error if a note commitment subtree is missing or incorrect.
fn quick_check_sapling_subtrees(db: &ZebraDb) -> Result<(), &'static str> {
    // We check the first sapling subtree on mainnet, so skip this check if it isn't available.
    if db.network() != Mainnet {
        return Ok(());
    }

    let Some(NoteCommitmentSubtreeIndex(tip_subtree_index)) =
        db.sapling_tree_for_tip().subtree_index()
    else {
        return Ok(());
    };

    if tip_subtree_index == 0 && !db.sapling_tree_for_tip().is_complete_subtree() {
        return Ok(());
    }

    // Find the first complete subtree: previous and current trees, and their end heights.
    let first_complete_subtree = db
        .sapling_tree_by_height_range(..)
        // We need both the tree and its previous tree for each shielded block.
        .tuple_windows()
        .map(|((prev_end_height, prev_tree), (end_height, tree))| {
            (prev_end_height, prev_tree, end_height, tree)
        })
        .find(|(_prev_end_height, prev_tree, _end_height, tree)| {
            tree.contains_new_subtree(prev_tree)
        });

    let Some((prev_end_height, prev_tree, end_height, tree)) = first_complete_subtree else {
        let result = Err("iterator did not find complete subtree, but the tree has it");
        error!(?result);
        return result;
    };

    // Creating this test vector involves a cryptographic check, so only do it once.
    let expected_subtree = first_sapling_mainnet_subtree();

    let db_subtree = calculate_sapling_subtree(db, prev_end_height, prev_tree, end_height, tree);

    if db_subtree != expected_subtree {
        let result = Err("first subtree did not match expected test vector");
        error!(?result, ?db_subtree, ?expected_subtree);
        return result;
    }

    Ok(())
}

/// Quickly check that the first calculated orchard subtree is correct.
///
/// This allows us to fail the upgrade quickly in tests and during development,
/// rather than waiting ~20 minutes to see if it failed.
///
/// Returns an error if a note commitment subtree is missing or incorrect.
fn quick_check_orchard_subtrees(db: &ZebraDb) -> Result<(), &'static str> {
    // We check the first orchard subtree on mainnet, so skip this check if it isn't available.
    if db.network() != Mainnet {
        return Ok(());
    }

    let Some(NoteCommitmentSubtreeIndex(tip_subtree_index)) =
        db.orchard_tree_for_tip().subtree_index()
    else {
        return Ok(());
    };

    if tip_subtree_index == 0 && !db.orchard_tree_for_tip().is_complete_subtree() {
        return Ok(());
    }

    // Find the first complete subtree: previous and current trees, and their end heights.
    let first_complete_subtree = db
        .orchard_tree_by_height_range(..)
        // We need both the tree and its previous tree for each shielded block.
        .tuple_windows()
        .map(|((prev_end_height, prev_tree), (end_height, tree))| {
            (prev_end_height, prev_tree, end_height, tree)
        })
        .find(|(_prev_end_height, prev_tree, _end_height, tree)| {
            tree.contains_new_subtree(prev_tree)
        });

    let Some((prev_end_height, prev_tree, end_height, tree)) = first_complete_subtree else {
        let result = Err("iterator did not find complete subtree, but the tree has it");
        error!(?result);
        return result;
    };

    // Creating this test vector involves a cryptographic check, so only do it once.
    let expected_subtree = first_orchard_mainnet_subtree();

    let db_subtree = calculate_orchard_subtree(db, prev_end_height, prev_tree, end_height, tree);

    if db_subtree != expected_subtree {
        let result = Err("first subtree did not match expected test vector");
        error!(?result, ?db_subtree, ?expected_subtree);
        return result;
    }

    Ok(())
}

/// Check that Sapling note commitment subtrees were correctly added.
///
/// Returns an error if a note commitment subtree is missing or incorrect.
fn check_sapling_subtrees(
    db: &ZebraDb,
    cancel_receiver: &Receiver<CancelFormatChange>,
) -> Result<Result<(), &'static str>, CancelFormatChange> {
    let Some(NoteCommitmentSubtreeIndex(mut first_incomplete_subtree_index)) =
        db.sapling_tree_for_tip().subtree_index()
    else {
        return Ok(Ok(()));
    };

    // If there are no incomplete subtrees in the tree, also expect a subtree for the final index.
    if db.sapling_tree_for_tip().is_complete_subtree() {
        first_incomplete_subtree_index += 1;
    }

    let mut result = Ok(());
    for index in 0..first_incomplete_subtree_index {
        // Return early if the format check is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // Check that there's a continuous range of subtrees from index [0, first_incomplete_subtree_index)
        let Some(subtree) = db.sapling_subtree_by_index(index) else {
            result = Err("missing subtree");
            error!(?result, index);
            continue;
        };

        // Check that there was a sapling note at the subtree's end height.
        let Some(tree) = db.sapling_tree_by_height(&subtree.end_height) else {
            result = Err("missing note commitment tree at subtree completion height");
            error!(?result, ?subtree.end_height);
            continue;
        };

        // Check the index and root if the sapling note commitment tree at this height is a complete subtree.
        if let Some((index, node)) = tree.completed_subtree_index_and_root() {
            if subtree.index != index {
                result = Err("completed subtree indexes should match");
                error!(?result);
            }

            if subtree.root != node {
                result = Err("completed subtree roots should match");
                error!(?result);
            }
        }
        // Check that the final note has a greater subtree index if it didn't complete a subtree.
        else {
            let prev_height = subtree
                .end_height
                .previous()
                .expect("Note commitment subtrees should not end at the minimal height.");

            let Some(prev_tree) = db.sapling_tree_by_height(&prev_height) else {
                result = Err("missing note commitment tree below subtree completion height");
                error!(?result, ?subtree.end_height);
                continue;
            };

            let prev_subtree_index = prev_tree.subtree_index();
            let subtree_index = tree.subtree_index();
            if subtree_index <= prev_subtree_index {
                result =
                    Err("note commitment tree at end height should have incremented subtree index");
                error!(?result, ?subtree_index, ?prev_subtree_index,);
            }
        }
    }

    let mut subtree_count = 0;
    for (index, height, tree) in db
        .sapling_tree_by_height_range(..)
        // Exclude empty sapling tree and add subtree indexes
        .filter_map(|(height, tree)| Some((tree.subtree_index()?, height, tree)))
        // Exclude heights that don't complete a subtree and count completed subtrees
        .filter_map(|(subtree_index, height, tree)| {
            if tree.is_complete_subtree() || subtree_index.0 > subtree_count {
                let subtree_index = subtree_count;
                subtree_count += 1;
                Some((subtree_index, height, tree))
            } else {
                None
            }
        })
    {
        // Return early if the format check is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // Check that there's an entry for every completed sapling subtree root in all sapling trees
        let Some(subtree) = db.sapling_subtree_by_index(index) else {
            result = Err("missing subtree");
            error!(?result, index);
            continue;
        };

        // Check that the subtree end height matches that in the sapling trees.
        if subtree.end_height != height {
            let is_complete = tree.is_complete_subtree();
            result = Err("bad sapling subtree end height");
            error!(?result, ?subtree.end_height, ?height, ?index, ?is_complete, );
        }

        // Check the root if the sapling note commitment tree at this height is a complete subtree.
        if let Some((_index, node)) = tree.completed_subtree_index_and_root() {
            if subtree.root != node {
                result = Err("completed subtree roots should match");
                error!(?result);
            }
        }
    }

    if result.is_err() {
        error!(
            ?result,
            ?subtree_count,
            first_incomplete_subtree_index,
            "missing or bad sapling subtrees"
        );
    }

    Ok(result)
}

/// Check that Orchard note commitment subtrees were correctly added.
///
/// Returns an error if a note commitment subtree is missing or incorrect.
fn check_orchard_subtrees(
    db: &ZebraDb,
    cancel_receiver: &Receiver<CancelFormatChange>,
) -> Result<Result<(), &'static str>, CancelFormatChange> {
    let Some(NoteCommitmentSubtreeIndex(mut first_incomplete_subtree_index)) =
        db.orchard_tree_for_tip().subtree_index()
    else {
        return Ok(Ok(()));
    };

    // If there are no incomplete subtrees in the tree, also expect a subtree for the final index.
    if db.orchard_tree_for_tip().is_complete_subtree() {
        first_incomplete_subtree_index += 1;
    }

    let mut result = Ok(());
    for index in 0..first_incomplete_subtree_index {
        // Return early if the format check is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // Check that there's a continuous range of subtrees from index [0, first_incomplete_subtree_index)
        let Some(subtree) = db.orchard_subtree_by_index(index) else {
            result = Err("missing subtree");
            error!(?result, index);
            continue;
        };

        // Check that there was a orchard note at the subtree's end height.
        let Some(tree) = db.orchard_tree_by_height(&subtree.end_height) else {
            result = Err("missing note commitment tree at subtree completion height");
            error!(?result, ?subtree.end_height);
            continue;
        };

        // Check the index and root if the orchard note commitment tree at this height is a complete subtree.
        if let Some((index, node)) = tree.completed_subtree_index_and_root() {
            if subtree.index != index {
                result = Err("completed subtree indexes should match");
                error!(?result);
            }

            if subtree.root != node {
                result = Err("completed subtree roots should match");
                error!(?result);
            }
        }
        // Check that the final note has a greater subtree index if it didn't complete a subtree.
        else {
            let prev_height = subtree
                .end_height
                .previous()
                .expect("Note commitment subtrees should not end at the minimal height.");

            let Some(prev_tree) = db.orchard_tree_by_height(&prev_height) else {
                result = Err("missing note commitment tree below subtree completion height");
                error!(?result, ?subtree.end_height);
                continue;
            };

            let prev_subtree_index = prev_tree.subtree_index();
            let subtree_index = tree.subtree_index();
            if subtree_index <= prev_subtree_index {
                result =
                    Err("note commitment tree at end height should have incremented subtree index");
                error!(?result, ?subtree_index, ?prev_subtree_index,);
            }
        }
    }

    let mut subtree_count = 0;
    for (index, height, tree) in db
        .orchard_tree_by_height_range(..)
        // Exclude empty orchard tree and add subtree indexes
        .filter_map(|(height, tree)| Some((tree.subtree_index()?, height, tree)))
        // Exclude heights that don't complete a subtree and count completed subtrees
        .filter_map(|(subtree_index, height, tree)| {
            if tree.is_complete_subtree() || subtree_index.0 > subtree_count {
                let subtree_index = subtree_count;
                subtree_count += 1;
                Some((subtree_index, height, tree))
            } else {
                None
            }
        })
    {
        // Return early if the format check is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // Check that there's an entry for every completed orchard subtree root in all orchard trees
        let Some(subtree) = db.orchard_subtree_by_index(index) else {
            result = Err("missing subtree");
            error!(?result, index);
            continue;
        };

        // Check that the subtree end height matches that in the orchard trees.
        if subtree.end_height != height {
            let is_complete = tree.is_complete_subtree();
            result = Err("bad orchard subtree end height");
            error!(?result, ?subtree.end_height, ?height, ?index, ?is_complete, );
        }

        // Check the root if the orchard note commitment tree at this height is a complete subtree.
        if let Some((_index, node)) = tree.completed_subtree_index_and_root() {
            if subtree.root != node {
                result = Err("completed subtree roots should match");
                error!(?result);
            }
        }
    }

    if result.is_err() {
        error!(
            ?result,
            ?subtree_count,
            first_incomplete_subtree_index,
            "missing or bad orchard subtrees"
        );
    }

    Ok(result)
}

/// Calculates a note commitment subtree for Sapling, reading blocks from `read_db` if needed.
///
/// The subtree must be completed by a note commitment in the block at `end_height`.
/// `tree` is the tree for that block, and `prev_tree` is the tree for the previous block.
///
/// `prev_tree` is only used to rebuild the subtree if it was completed without using the last
/// note commitment in the block at `end_height`.
///
/// # Panics
///
/// If a subtree is not completed by a note commitment in the block at `end_height`.
#[must_use = "subtree should be written to the database after it is calculated"]
#[instrument(skip(read_db, prev_tree, tree))]
fn calculate_sapling_subtree(
    read_db: &ZebraDb,
    prev_end_height: Height,
    prev_tree: Arc<sapling::tree::NoteCommitmentTree>,
    end_height: Height,
    tree: Arc<sapling::tree::NoteCommitmentTree>,
) -> NoteCommitmentSubtree<sapling_crypto::Node> {
    // If a subtree is completed by a note commitment in the block at `end_height`,
    // then that subtree can be completed in two different ways:
    if let Some((index, node)) = tree.completed_subtree_index_and_root() {
        // If the subtree is completed by the last note commitment in that block,
        // we already have that subtree root available in the tree.
        NoteCommitmentSubtree::new(index, end_height, node)
    } else {
        // If the subtree is completed without using the last note commitment in the block,
        // we need to calculate the subtree root, starting with the tree from the previous block.

        // TODO: move the assertion/panic log string formatting into a separate function?
        let prev_position = prev_tree.position().unwrap_or_else(|| {
            panic!(
                "previous block must have a partial subtree:\n\
                previous subtree:\n\
                height: {prev_end_height:?}\n\
                current subtree:\n\
                height: {end_height:?}"
            )
        });
        let prev_index = prev_tree
            .subtree_index()
            .expect("previous block must have a partial subtree");
        let prev_remaining_notes = prev_tree.remaining_subtree_leaf_nodes();

        let current_position = tree.position().unwrap_or_else(|| {
            panic!(
                "current block must have a subtree:\n\
                previous subtree:\n\
                height: {prev_end_height:?}\n\
                index: {prev_index}\n\
                position: {prev_position}\n\
                remaining: {prev_remaining_notes}\n\
                current subtree:\n\
                height: {end_height:?}"
            )
        });
        let current_index = tree
            .subtree_index()
            .expect("current block must have a subtree");
        let current_remaining_notes = tree.remaining_subtree_leaf_nodes();

        assert_eq!(
            prev_index.0 + 1,
            current_index.0,
            "subtree must have been completed by the current block:\n\
             previous subtree:\n\
             height: {prev_end_height:?}\n\
             index: {prev_index}\n\
             position: {prev_position}\n\
             remaining: {prev_remaining_notes}\n\
             current subtree:\n\
             height: {end_height:?}\n\
             index: {current_index}\n\
             position: {current_position}\n\
             remaining: {current_remaining_notes}"
        );

        // Get the missing notes needed to complete the subtree.
        //
        // TODO: consider just reading the block's transactions from the database file,
        //       because we don't use the block header data at all.
        let block = read_db
            .block(end_height.into())
            .expect("height with note commitment tree should have block");
        let sapling_note_commitments = block
            .sapling_note_commitments()
            .take(prev_remaining_notes)
            .cloned()
            .collect();

        // This takes less than 1 second per tree, so we don't need to make it cancellable.
        let (sapling_nct, subtree) = NoteCommitmentTrees::update_sapling_note_commitment_tree(
            prev_tree,
            sapling_note_commitments,
        )
        .expect("finalized notes should append successfully");

        let (index, node) = subtree.unwrap_or_else(|| {
            panic!(
                "already checked that the block completed a subtree:\n\
                 updated subtree:\n\
                 index: {:?}\n\
                 position: {:?}\n\
                 remaining notes: {}\n\
                 original previous subtree:\n\
                 height: {prev_end_height:?}\n\
                 index: {prev_index}\n\
                 position: {prev_position}\n\
                 remaining: {prev_remaining_notes}\n\
                 original current subtree:\n\
                 height: {end_height:?}\n\
                 index: {current_index}\n\
                 position: {current_position}\n\
                 remaining: {current_remaining_notes}",
                sapling_nct.subtree_index(),
                sapling_nct.position(),
                sapling_nct.remaining_subtree_leaf_nodes(),
            )
        });

        NoteCommitmentSubtree::new(index, end_height, node)
    }
}

/// Calculates a note commitment subtree for Orchard, reading blocks from `read_db` if needed.
///
/// The subtree must be completed by a note commitment in the block at `end_height`.
/// `tree` is the tree for that block, and `prev_tree` is the tree for the previous block.
///
/// `prev_tree` is only used to rebuild the subtree if it was completed without using the last
/// note commitment in the block at `end_height`.
///
/// # Panics
///
/// If a subtree is not completed by a note commitment in the block at `end_height`.
#[must_use = "subtree should be written to the database after it is calculated"]
#[instrument(skip(read_db, prev_tree, tree))]
fn calculate_orchard_subtree(
    read_db: &ZebraDb,
    prev_end_height: Height,
    prev_tree: Arc<orchard::tree::NoteCommitmentTree>,
    end_height: Height,
    tree: Arc<orchard::tree::NoteCommitmentTree>,
) -> NoteCommitmentSubtree<orchard::tree::Node> {
    // If a subtree is completed by a note commitment in the block at `end_height`,
    // then that subtree can be completed in two different ways:
    if let Some((index, node)) = tree.completed_subtree_index_and_root() {
        // If the subtree is completed by the last note commitment in that block,
        // we already have that subtree root available in the tree.
        NoteCommitmentSubtree::new(index, end_height, node)
    } else {
        // If the subtree is completed without using the last note commitment in the block,
        // we need to calculate the subtree root, starting with the tree from the previous block.

        // TODO: move the assertion/panic log string formatting into a separate function?
        let prev_position = prev_tree.position().unwrap_or_else(|| {
            panic!(
                "previous block must have a partial subtree:\n\
                previous subtree:\n\
                height: {prev_end_height:?}\n\
                current subtree:\n\
                height: {end_height:?}"
            )
        });
        let prev_index = prev_tree
            .subtree_index()
            .expect("previous block must have a partial subtree");
        let prev_remaining_notes = prev_tree.remaining_subtree_leaf_nodes();

        let current_position = tree.position().unwrap_or_else(|| {
            panic!(
                "current block must have a subtree:\n\
                previous subtree:\n\
                height: {prev_end_height:?}\n\
                index: {prev_index}\n\
                position: {prev_position}\n\
                remaining: {prev_remaining_notes}\n\
                current subtree:\n\
                height: {end_height:?}"
            )
        });
        let current_index = tree
            .subtree_index()
            .expect("current block must have a subtree");
        let current_remaining_notes = tree.remaining_subtree_leaf_nodes();

        assert_eq!(
            prev_index.0 + 1,
            current_index.0,
            "subtree must have been completed by the current block:\n\
             previous subtree:\n\
             height: {prev_end_height:?}\n\
             index: {prev_index}\n\
             position: {prev_position}\n\
             remaining: {prev_remaining_notes}\n\
             current subtree:\n\
             height: {end_height:?}\n\
             index: {current_index}\n\
             position: {current_position}\n\
             remaining: {current_remaining_notes}"
        );

        // Get the missing notes needed to complete the subtree.
        //
        // TODO: consider just reading the block's transactions from the database file,
        //       because we don't use the block header data at all.
        let block = read_db
            .block(end_height.into())
            .expect("height with note commitment tree should have block");
        let orchard_note_commitments = block
            .orchard_note_commitments()
            .take(prev_remaining_notes)
            .cloned()
            .collect();

        // This takes less than 1 second per tree, so we don't need to make it cancellable.
        let (orchard_nct, subtree) = NoteCommitmentTrees::update_orchard_note_commitment_tree(
            prev_tree,
            orchard_note_commitments,
        )
        .expect("finalized notes should append successfully");

        let (index, node) = subtree.unwrap_or_else(|| {
            panic!(
                "already checked that the block completed a subtree:\n\
                 updated subtree:\n\
                 index: {:?}\n\
                 position: {:?}\n\
                 remaining notes: {}\n\
                 original previous subtree:\n\
                 height: {prev_end_height:?}\n\
                 index: {prev_index}\n\
                 position: {prev_position}\n\
                 remaining: {prev_remaining_notes}\n\
                 original current subtree:\n\
                 height: {end_height:?}\n\
                 index: {current_index}\n\
                 position: {current_position}\n\
                 remaining: {current_remaining_notes}",
                orchard_nct.subtree_index(),
                orchard_nct.position(),
                orchard_nct.remaining_subtree_leaf_nodes(),
            )
        });

        NoteCommitmentSubtree::new(index, end_height, node)
    }
}

/// Writes a Sapling note commitment subtree to `upgrade_db`.
fn write_sapling_subtree(
    upgrade_db: &ZebraDb,
    subtree: NoteCommitmentSubtree<sapling_crypto::Node>,
) {
    let mut batch = DiskWriteBatch::new();

    batch.insert_sapling_subtree(upgrade_db, &subtree);

    upgrade_db
        .write_batch(batch)
        .expect("writing sapling note commitment subtrees should always succeed.");

    if subtree.index.0.is_multiple_of(100) {
        info!(end_height = ?subtree.end_height, index = ?subtree.index.0, "calculated and added sapling subtree");
    }
    // This log happens about once per second on recent machines with SSD disks.
    debug!(end_height = ?subtree.end_height, index = ?subtree.index.0, "calculated and added sapling subtree");
}

/// Writes an Orchard note commitment subtree to `upgrade_db`.
fn write_orchard_subtree(
    upgrade_db: &ZebraDb,
    subtree: NoteCommitmentSubtree<orchard::tree::Node>,
) {
    let mut batch = DiskWriteBatch::new();

    batch.insert_orchard_subtree(upgrade_db, &subtree);

    upgrade_db
        .write_batch(batch)
        .expect("writing orchard note commitment subtrees should always succeed.");

    if subtree.index.0.is_multiple_of(100) {
        info!(end_height = ?subtree.end_height, index = ?subtree.index.0, "calculated and added orchard subtree");
    }
    // This log happens about once per second on recent machines with SSD disks.
    debug!(end_height = ?subtree.end_height, index = ?subtree.index.0, "calculated and added orchard subtree");
}
