//! Backfills per-block synchronization metadata for existing blocks.
//!
//! The `sync_meta_by_height` column family stores each block's size value,
//! transaction and note commitment counts, ZIP 221 per-pool transaction
//! counts, ZIP 244 authorizing data commitment, and cumulative transparent
//! output count. New blocks write it at commit time; this upgrade computes
//! it for blocks committed by earlier Zebra versions.

use crossbeam_channel::{Receiver, TryRecvError};
use semver::Version;
use zebra_chain::block::Height;

use crate::service::finalized_state::{disk_format::chain::SyncMetadata, DiskWriteBatch, ZebraDb};

use super::{CancelFormatChange, DiskFormatUpgrade};

/// The number of blocks per write batch and cancellation check.
const BATCH_HEIGHTS: u32 = 2_000;

/// Implements [`DiskFormatUpgrade`] for backfilling per-block
/// synchronization metadata.
pub struct Upgrade;

impl DiskFormatUpgrade for Upgrade {
    fn version(&self) -> Version {
        Version::new(28, 1, 0)
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
        // still be missed; `validate` then fails, and the upgrade re-runs at
        // the next startup, because the format version is only bumped once
        // the backfill is complete.
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

                for height in next_height..batch_end {
                    let height = Height(height);
                    let (block, size) = db
                        .block_and_size(height.into())
                        .expect("blocks below the observed tip height exist");

                    let metadata =
                        SyncMetadata::for_block(&block, size, cumulative_transparent_outputs);
                    cumulative_transparent_outputs = metadata.cumulative_transparent_outputs;

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

        if db.sync_metadata(tip_height).is_none() {
            return Ok(Err(
                "missing synchronization metadata for the finalized tip: \
                 a block commit raced the end of the backfill; \
                 restart Zebra to complete the format upgrade"
                    .to_string(),
            ));
        }

        Ok(Ok(()))
    }
}
