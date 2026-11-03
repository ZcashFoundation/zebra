//! Backfill the genesis Ironwood note commitment tree and anchor for existing databases.
//!
//! The NU6.3 Ironwood column families are created empty when a v28 database is opened. A node that
//! synced from genesis under v28 writes the (empty) Ironwood tree and its anchor at the genesis
//! height during the genesis block commit. A node upgrading an older database committed its genesis
//! block long ago, under code that never wrote an Ironwood tree, so the `ironwood_note_commitment_tree`
//! and `ironwood_anchors` column families would otherwise stay empty — which makes
//! [`ZebraDb::ironwood_tree_for_tip`](crate::service::finalized_state::ZebraDb::ironwood_tree_for_tip)
//! panic on the next block and leaves the genesis Ironwood anchor missing for NU6.3 anchor
//! validation.
//!
//! This migration backfills the empty Ironwood tree (and its anchor) at the genesis height so the
//! on-disk state matches a genesis-synced node. It is a no-op for genesis-synced and already-upgraded
//! databases. The Ironwood pool reuses the Orchard note commitment tree type.

use crossbeam_channel::{Receiver, TryRecvError};
use semver::Version;

use zebra_chain::{block::Height, orchard};

use crate::service::finalized_state::{DiskWriteBatch, ZebraDb};

use super::{CancelFormatChange, DiskFormatUpgrade};

/// Implements [`DiskFormatUpgrade`] for backfilling the genesis Ironwood tree and anchor.
pub struct Upgrade;

impl DiskFormatUpgrade for Upgrade {
    fn version(&self) -> Version {
        Version::new(28, 0, 0)
    }

    fn description(&self) -> &'static str {
        "add ironwood shielded pool state (genesis tree and anchor backfill)"
    }

    #[allow(clippy::unwrap_in_result)]
    fn run(
        &self,
        _initial_tip_height: Height,
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        check_cancelled(cancel_receiver)?;

        // Nothing to do for empty databases, or databases that already have the genesis tree
        // (genesis-synced under v28, or a previous run of this migration).
        if db.finalized_tip_height().is_none() || has_genesis_ironwood_tree(db) {
            return Ok(());
        }

        // Write the empty Ironwood tree (and its anchor) at the genesis height, matching what a
        // genesis-synced v28 node writes during the genesis block commit.
        let ironwood_tree = orchard::tree::NoteCommitmentTree::default();
        let mut batch = DiskWriteBatch::new();
        batch.create_ironwood_tree(db, &Height::MIN, &ironwood_tree);

        check_cancelled(cancel_receiver)?;

        db.write_batch(batch)
            .expect("backfilling the genesis Ironwood tree should always succeed");

        Ok(())
    }

    fn validate(
        &self,
        db: &ZebraDb,
        _cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<Result<(), String>, CancelFormatChange> {
        // Empty databases have no genesis tree to check.
        if db.finalized_tip_height().is_none() {
            return Ok(Ok(()));
        }

        if !has_genesis_ironwood_tree(db) {
            return Ok(Err(
                "missing Ironwood note commitment tree for the genesis height".to_string(),
            ));
        }

        let ironwood_tree = db.ironwood_tree_for_tip();
        if !db.contains_ironwood_anchor(&ironwood_tree.root()) {
            return Ok(Err(
                "missing Ironwood anchor for the finalized tip's Ironwood tree".to_string(),
            ));
        }

        Ok(Ok(()))
    }
}

/// Returns `true` if the database has an Ironwood note commitment tree at or below the genesis height.
fn has_genesis_ironwood_tree(db: &ZebraDb) -> bool {
    db.ironwood_tree_by_height_range(..=Height::MIN)
        .next()
        .is_some()
}

fn check_cancelled(
    cancel_receiver: &Receiver<CancelFormatChange>,
) -> Result<(), CancelFormatChange> {
    match cancel_receiver.try_recv() {
        Err(TryRecvError::Empty) => Ok(()),
        _ => Err(CancelFormatChange),
    }
}
