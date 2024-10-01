//! Tracks transaction locations by their inputs and revealed nullifiers.

use std::sync::mpsc;

use zebra_chain::{block::Height, transaction::Transaction};

use crate::{service::finalized_state::ZebraDb, TransactionIndex, TransactionLocation};

use super::CancelFormatChange;

/// The number of transactions to process before writing a batch to disk.
const WRITE_BATCH_SIZE: usize = 1_000;

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
    let db = zebra_db.db();
    let tx_by_loc = db.cf_handle("tx_by_loc").unwrap();
    let end_bound = TransactionLocation::from_parts(initial_tip_height, TransactionIndex::MAX);

    let transactions_iter = db
        .zs_forward_range_iter::<_, _, Transaction, _>(tx_by_loc, ..=end_bound)
        .filter(|(_, tx)| !tx.is_coinbase())
        .map(|(tx_loc, tx)| (tx_loc, tx.inputs().to_vec()));

    let new_batch = || {
        zebra_db
            .tx_loc_by_spent_output_loc_cf()
            .new_batch_for_writing()
    };

    let mut batch = new_batch();
    let mut num_inputs_processed_since_write: usize = 0;
    for (tx_loc, inputs) in transactions_iter {
        // Return before I/O calls if the upgrade is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        for input in inputs {
            let spent_outpoint = input
                .outpoint()
                .expect("should filter out coinbase transactions");
            let spent_output_location = zebra_db
                .output_location(&spent_outpoint)
                .expect("should have location for spent outpoint");

            batch = batch.zs_insert(&spent_output_location, &tx_loc);
            num_inputs_processed_since_write += 1;
        }

        // Return before I/O calls if the upgrade is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        if num_inputs_processed_since_write >= WRITE_BATCH_SIZE {
            std::mem::replace(&mut batch, new_batch())
                .write_batch()
                .expect("unexpected database write failure");
            num_inputs_processed_since_write = 0;
        }
    }

    batch
        .write_batch()
        .expect("unexpected database write failure");

    Ok(())
}
