//! Backfills per-block synchronization metadata for existing blocks.
//!
//! The `sync_meta_by_height` column family stores each block's serialized
//! size, transaction and note commitment counts, ZIP 221 per-pool transaction
//! counts, ZIP 244 authorizing data commitment, and cumulative transparent
//! output count. New blocks write it at commit time; this upgrade computes it
//! for blocks committed by earlier Zebra versions.
//!
//! Reading and deserializing each block is most of the upgrade's work, so the
//! blocks of each batch are read and measured in parallel on the rayon pool,
//! and only the cumulative output count is summed in height order.

use crossbeam_channel::{Receiver, TryRecvError};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use semver::Version;
use zebra_chain::block::Height;

use crate::service::finalized_state::{
    disk_format::chain::SyncMetadata, zebra_db::sync_meta::MAX_SYNC_METADATA_TAIL_GAP,
    DiskWriteBatch, ZebraDb,
};

use super::{CancelFormatChange, DiskFormatUpgrade};

/// The number of blocks per write batch and cancellation check. Each batch's
/// blocks are read in parallel, and only one batch of records is held in
/// memory, 64 bytes per block.
const BATCH_HEIGHTS: u32 = 2_000;

/// Implements [`DiskFormatUpgrade`] for backfilling per-block
/// synchronization metadata.
pub struct Upgrade;

impl DiskFormatUpgrade for Upgrade {
    fn version(&self) -> Version {
        Version::new(29, 1, 0)
    }

    fn description(&self) -> &'static str {
        "add per-block synchronization metadata (backfill)"
    }

    #[allow(clippy::unwrap_in_result)]
    fn run(
        &self,
        initial_tip_height: Height,
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        // Metadata is written in height order, and commit-time writes are
        // skipped while a parent record is missing, so the column family is
        // always contiguous from genesis. A previous cancelled run therefore
        // resumes after the highest height it wrote, seeding the cumulative
        // transparent output count from that record.
        let (mut next_height, mut cumulative_transparent_outputs) = db
            .sync_meta_cf()
            .zs_last_key_value()
            .map(|(height, metadata)| (height.0 + 1, metadata.cumulative_transparent_outputs))
            .unwrap_or((0, 0));

        // The backfill chases the live tip, not just the tip at upgrade
        // start: a block that commits while the backfill runs skips its
        // commit-time metadata write when its parent has no record yet, and
        // is picked up by a later pass here instead.
        //
        // A block whose commit races the final tip observation below can
        // still be missed. The next block commit fills that short gap (see
        // `zebra_db::sync_meta`), and `validate` tolerates it.
        let mut tip_height = initial_tip_height;
        loop {
            while next_height <= tip_height.0 {
                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                let mut batch = DiskWriteBatch::new();
                let batch_end = next_height
                    .saturating_add(BATCH_HEIGHTS)
                    .min(tip_height.0 + 1);

                // Each record is computed as if its parent's cumulative count were zero, so its
                // cumulative count is the block's own transparent output count. The auth data
                // root can't be read from anywhere cheaper: it is only stored hashed into the
                // header's block commitments.
                let records: Vec<(Height, SyncMetadata)> = (next_height..batch_end)
                    .into_par_iter()
                    .map(|height| {
                        let height = Height(height);
                        let (block, size) = db.block_and_size(height.into()).ok().flatten().expect(
                            "blocks below the observed tip height exist, and are not pruned \
                             because pruning needs format 31, which runs after this upgrade",
                        );

                        (height, SyncMetadata::for_block(&block, size, 0, None))
                    })
                    .collect();

                for (height, mut metadata) in records {
                    cumulative_transparent_outputs = cumulative_transparent_outputs
                        .checked_add(metadata.cumulative_transparent_outputs)
                        .expect(
                            "a u32-height chain of MAX_BLOCK_BYTES blocks of minimal outputs \
                             stays below u64::MAX outputs",
                        );
                    metadata.cumulative_transparent_outputs = cumulative_transparent_outputs;

                    let _ = db
                        .sync_meta_cf()
                        .with_batch_for_writing(&mut batch)
                        .zs_insert(&height, &metadata);
                }

                db.write_batch(batch)
                    .expect("writing synchronization metadata should always succeed");
                next_height = batch_end;
            }

            // Re-read the tip: blocks that committed during the pass above
            // may have skipped their own metadata writes.
            let live_tip = db.finalized_tip_height().unwrap_or(tip_height);
            if live_tip.0 < next_height {
                break;
            }
            tip_height = live_tip;
        }

        Ok(())
    }

    fn validate(
        &self,
        db: &ZebraDb,
        _cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<Result<(), String>, CancelFormatChange> {
        let Some(tip_height) = db.finalized_tip_height() else {
            return Ok(Ok(()));
        };

        let last_height = db
            .sync_meta_cf()
            .zs_last_key_value()
            .map(|(height, _)| height);

        // A database imported from a state snapshot has no blocks below its window, so it has no
        // synchronization metadata at all: records start at genesis, and commits above the
        // snapshot have no parent record to continue from. Its `get-hashes` answers are empty.
        if last_height.is_none() && db.snapshot_base().is_some() {
            return Ok(Ok(()));
        }

        // A commit that raced the end of the backfill can leave a short gap
        // below the tip, which the next commit fills.
        let lowest_valid_last_height =
            Height(tip_height.0.saturating_sub(MAX_SYNC_METADATA_TAIL_GAP));
        if last_height.is_none_or(|last_height| last_height < lowest_valid_last_height) {
            return Ok(Err(format!(
                "missing synchronization metadata below the finalized tip: \
                 last record: {last_height:?}, finalized tip: {tip_height:?}"
            )));
        }

        // Records are contiguous from genesis.
        let first_height = db.sync_meta_cf().zs_first_key_value().map(|(h, _)| h);
        if first_height != Some(Height(0)) {
            return Ok(Err(format!(
                "synchronization metadata must start at genesis, first record: {first_height:?}"
            )));
        }

        Ok(Ok(()))
    }
}
