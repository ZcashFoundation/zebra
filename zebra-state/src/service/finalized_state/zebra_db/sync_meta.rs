//! The `sync_meta_by_height` column family (format 29.1.0; design §5).
//!
//! Written in the block batch for every committed block, and backfilled for
//! existing databases by the 29.1.0 format upgrade. The `known_hash_chunk`
//! CF of #11349 is not ported.
//!
//! # Contiguity
//!
//! Records are always contiguous from genesis: each record stores the
//! cumulative transparent output count, so a record can only be computed
//! from its parent's record. While the format upgrade is still backfilling,
//! a committed block whose parent has no record skips its own write, and the
//! backfill fills it in later.
//!
//! A commit that races the end of the backfill can still leave the newest
//! heights without records. The next commit closes such a gap of at most
//! [`MAX_SYNC_METADATA_TAIL_GAP`] heights in its own batch, so the column
//! family catches up with the tip without a restart.

use std::ops::RangeInclusive;

use zebra_chain::block::Height;

use zebra_db::FinalizedBlock;

use crate::service::finalized_state::{
    disk_db::DiskWriteBatch, disk_format::chain::SyncMetadata, TypedColumnFamily, ZebraDb,
};

#[cfg(test)]
mod tests;

/// The column family name: `Height` → [`SyncMetadata`].
pub const SYNC_META_BY_HEIGHT: &str = "sync_meta_by_height";

/// The type for reading per-block synchronization metadata from the database.
///
/// This type should be used so the compiler can detect incorrectly typed
/// accesses to the column family.
pub type SyncMetaCf<'cf> = TypedColumnFamily<'cf, Height, SyncMetadata>;

/// The largest run of missing heights directly below a committed block that
/// the commit fills in its own batch.
///
/// The finalized writer commits one block at a time, so a commit racing the
/// end of the backfill leaves a gap of at most one height. A larger gap
/// means the backfill is still running, and the commit leaves it to the
/// backfill instead of reading many blocks on the writer thread.
pub const MAX_SYNC_METADATA_TAIL_GAP: u32 = 32;

impl ZebraDb {
    /// Returns a typed handle to the `sync_meta_by_height` column family.
    pub(crate) fn sync_meta_cf(&self) -> SyncMetaCf<'_> {
        SyncMetaCf::new(&self.db, SYNC_META_BY_HEIGHT)
            .expect("column family was created when database was created")
    }

    /// The sync metadata of the finalized block at `height`, or `None` above
    /// the finalized tip or before the 29.1.0 upgrade reached `height`.
    pub fn sync_metadata(&self, height: Height) -> Option<SyncMetadata> {
        self.sync_meta_cf().zs_get(&height)
    }

    /// The sync metadata for `range`, in height order: one range scan,
    /// stopping at the first missing height (so the result is always a
    /// contiguous prefix of `range`).
    pub fn sync_metadata_range(&self, range: RangeInclusive<Height>) -> Vec<SyncMetadata> {
        let mut expected_height = Some(*range.start());

        self.sync_meta_cf()
            .zs_forward_range_iter(range)
            .map_while(|(height, metadata)| {
                if Some(height) != expected_height {
                    return None;
                }
                expected_height = height.next().ok();
                Some(metadata)
            })
            .collect()
    }

    /// Whether every finalized height has sync metadata (the 29.1.0 upgrade
    /// has finished). Until it has, serving `get-hashes` stops at the first
    /// missing height.
    pub fn sync_metadata_is_complete(&self) -> bool {
        let Some(tip_height) = self.finalized_tip_height() else {
            return true;
        };

        // Records are contiguous from genesis, so the last record is at the
        // tip exactly when every height has one.
        self.sync_meta_cf()
            .zs_last_key_value()
            .is_some_and(|(height, _)| height >= tip_height)
    }
}

impl DiskWriteBatch {
    /// Prepares the sync metadata record of `finalized`, whose serialized size
    /// is `block_size`, in this batch.
    ///
    /// Skips the write while the format upgrade is still backfilling below
    /// this block, and fills a small gap directly below it (see the module
    /// docs).
    pub fn prepare_sync_metadata_batch(
        &mut self,
        db: &ZebraDb,
        finalized: &FinalizedBlock,
        block_size: usize,
    ) {
        let height = finalized.height;

        // Genesis seeds the cumulative transparent output count from zero.
        let prev_cumulative_transparent_outputs = match height.previous() {
            Err(_genesis_has_no_previous) => 0,
            Ok(parent_height) => match db.sync_metadata(parent_height) {
                Some(parent) => parent.cumulative_transparent_outputs,
                None => match self.fill_sync_metadata_tail_gap(db, parent_height) {
                    Some(cumulative_transparent_outputs) => cumulative_transparent_outputs,
                    None => return,
                },
            },
        };

        let _ = db.sync_meta_cf().with_batch_for_writing(self).zs_insert(
            &height,
            &SyncMetadata::for_block(
                &finalized.block,
                block_size,
                prev_cumulative_transparent_outputs,
                finalized.auth_data_root,
            ),
        );
    }

    /// Writes the missing records up to and including `parent_height` in this
    /// batch, if the gap is at most [`MAX_SYNC_METADATA_TAIL_GAP`] heights,
    /// and returns the parent's cumulative transparent output count.
    ///
    /// Returns `None` and writes nothing if the gap is larger, or if there are
    /// no records at all (the backfill hasn't written genesis yet).
    #[allow(clippy::unwrap_in_result)]
    fn fill_sync_metadata_tail_gap(&mut self, db: &ZebraDb, parent_height: Height) -> Option<u64> {
        let (last_height, last) = db.sync_meta_cf().zs_last_key_value()?;

        // The parent is missing, and records are contiguous from genesis, so
        // the last record is below the parent.
        let gap = parent_height.0.checked_sub(last_height.0)?;
        if gap > MAX_SYNC_METADATA_TAIL_GAP {
            return None;
        }

        let mut cumulative_transparent_outputs = last.cumulative_transparent_outputs;
        for height in (last_height.0 + 1)..=parent_height.0 {
            let height = Height(height);
            let (block, size) = db
                .block_and_size(height.into())
                // A pruned body leaves the tail without metadata.
                .ok()?
                .expect("blocks at or below the parent of a committing block are finalized");

            let metadata =
                SyncMetadata::for_block(&block, size, cumulative_transparent_outputs, None);
            cumulative_transparent_outputs = metadata.cumulative_transparent_outputs;

            let _ = db
                .sync_meta_cf()
                .with_batch_for_writing(self)
                .zs_insert(&height, &metadata);
        }

        Some(cumulative_transparent_outputs)
    }
}
