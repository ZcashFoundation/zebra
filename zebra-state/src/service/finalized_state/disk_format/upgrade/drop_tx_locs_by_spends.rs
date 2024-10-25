//! Tracks transaction locations by their inputs and revealed nullifiers.

use crossbeam_channel::{Receiver, TryRecvError};
use rayon::iter::{IntoParallelIterator, ParallelIterator};

use zebra_chain::block::Height;

use crate::service::finalized_state::ZebraDb;

use super::{super::super::DiskWriteBatch, CancelFormatChange};

/// Runs disk format upgrade for tracking transaction locations by their inputs and revealed nullifiers.
///
/// Returns `Ok` if the upgrade completed, and `Err` if it was cancelled.
#[allow(clippy::unwrap_in_result)]
#[instrument(skip(zebra_db, cancel_receiver))]
pub fn run(
    initial_tip_height: Height,
    zebra_db: &ZebraDb,
    cancel_receiver: &Receiver<CancelFormatChange>,
) -> Result<(), CancelFormatChange> {
    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
        return Err(CancelFormatChange);
    }

    // The `TX_LOC_BY_SPENT_OUT_LOC` column family should be dropped
    // when opening the database without the `indexer` feature.

    (0..=initial_tip_height.0)
        .into_par_iter()
        .try_for_each(|height| {
            let height = Height(height);
            let mut batch = DiskWriteBatch::new();

            for (_tx_loc, tx) in zebra_db.transactions_by_height(height) {
                if tx.is_coinbase() {
                    continue;
                }

                batch
                    .prepare_nullifier_batch(zebra_db, &tx)
                    .expect("method should never return an error");
            }

            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            zebra_db
                .write_batch(batch)
                .expect("unexpected database write failure");

            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            Ok(())
        })
}
