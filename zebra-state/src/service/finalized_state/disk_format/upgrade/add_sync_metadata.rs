//! Backfills per-block synchronization metadata for existing blocks.
//!
//! The `sync_meta_by_height` column family stores each block's size value,
//! transaction and note commitment counts, ZIP 221 per-pool transaction
//! counts, and ZIP 244 authorizing data commitment. New blocks write it at
//! commit time; this upgrade computes it for blocks committed by earlier
//! Zebra versions.

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
        // Metadata is written in height order, so a previous cancelled run
        // resumes after the highest height it wrote.
        let mut next_height = db
            .sync_meta_cf()
            .zs_last_key_value()
            .map(|(height, _)| height.0 + 1)
            .unwrap_or(0);

        while next_height <= initial_tip_height.0 {
            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            let mut batch = DiskWriteBatch::new();
            let batch_end = next_height
                .saturating_add(BATCH_HEIGHTS)
                .min(initial_tip_height.0 + 1);

            for height in next_height..batch_end {
                let height = Height(height);
                let (block, size) = db
                    .block_and_size(height.into())
                    .expect("blocks below the initial tip height exist");

                let _ = db
                    .sync_meta_cf()
                    .with_batch_for_writing(&mut batch)
                    .zs_insert(&height, &SyncMetadata::for_block(&block, size));
            }

            db.write_batch(batch)
                .expect("writing synchronization metadata should always succeed");
            next_height = batch_end;
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
                "missing synchronization metadata for the finalized tip".to_string(),
            ));
        }

        Ok(Ok(()))
    }
}
