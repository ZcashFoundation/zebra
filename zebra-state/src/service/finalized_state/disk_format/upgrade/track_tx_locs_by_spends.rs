//! Tracks transaction locations by their inputs and revealed nullifiers.

use std::sync::mpsc;

use zebra_chain::block::Height;

use crate::{service::finalized_state::ZebraDb, TransactionIndex, TransactionLocation};

use super::super::super::DiskWriteBatch;
use super::CancelFormatChange;

/// Runs disk format upgrade for tracking transaction locations by their inputs and revealed nullifiers.
///
/// Returns `Ok` if the upgrade completed, and `Err` if it was cancelled.
#[allow(clippy::unwrap_in_result)]
#[instrument(skip(zebra_db, cancel_receiver))]
pub fn run(
    initial_tip_height: Height,
    zebra_db: &ZebraDb,
    cancel_receiver: &mpsc::Receiver<CancelFormatChange>,
) -> Result<(), CancelFormatChange> {
    if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
        return Err(CancelFormatChange);
    }

    let end_bound = TransactionLocation::from_parts(initial_tip_height, TransactionIndex::MAX);

    for (tx_loc, tx) in zebra_db
        .transactions_by_location_range(..=end_bound)
        .filter(|(_, tx)| !tx.is_coinbase())
    {
        let mut batch = DiskWriteBatch::new();

        for input in tx.inputs() {
            if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            let spent_outpoint = input
                .outpoint()
                .expect("should filter out coinbase transactions");

            let spent_output_location = zebra_db
                .output_location(&spent_outpoint)
                .expect("should have location for spent outpoint");

            let _ = zebra_db
                .tx_loc_by_spent_output_loc_cf()
                .with_batch_for_writing(&mut batch)
                .zs_insert(&spent_output_location, &tx_loc);
        }

        batch
            .prepare_nullifier_batch(zebra_db, &tx, tx_loc)
            .expect("should not return an error");

        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        zebra_db
            .write_batch(batch)
            .expect("unexpected database write failure");

        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }
    }

    Ok(())
}
