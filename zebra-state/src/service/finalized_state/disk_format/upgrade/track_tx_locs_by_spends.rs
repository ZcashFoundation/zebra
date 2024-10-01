//! Tracks transaction locations by their inputs and revealed nullifiers.

use std::sync::mpsc;

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use zebra_chain::{block::Height, transaction::Transaction};

use crate::{service::finalized_state::ZebraDb, TransactionIndex, TransactionLocation};

use super::super::super::{DiskWriteBatch, WriteDisk};
use super::CancelFormatChange;

/// The number of transactions to process before writing a batch to disk.
const WRITE_BATCH_SIZE: usize = 10_000;

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
    let sprout_nullifiers = db.cf_handle("sprout_nullifiers").unwrap();
    let sapling_nullifiers = db.cf_handle("sapling_nullifiers").unwrap();
    let orchard_nullifiers = db.cf_handle("orchard_nullifiers").unwrap();
    let end_bound = TransactionLocation::from_parts(initial_tip_height, TransactionIndex::MAX);

    let transactions_iter = db
        .zs_forward_range_iter::<_, _, Transaction, _>(tx_by_loc, ..=end_bound)
        .filter(|(_, tx)| !tx.is_coinbase());

    let mut batch = DiskWriteBatch::new();
    let mut num_ops_in_write_batch: usize = 0;
    for (tx_loc, tx) in transactions_iter {
        // Return before I/O calls if the upgrade is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // read spent outpoints' output locations in parallel
        let spent_output_locations: Vec<_> = tx
            .inputs()
            .par_iter()
            .map(|input| {
                let spent_outpoint = input
                    .outpoint()
                    .expect("should filter out coinbase transactions");

                zebra_db
                    .output_location(&spent_outpoint)
                    .expect("should have location for spent outpoint")
            })
            .collect();

        for spent_output_location in spent_output_locations {
            let _ = zebra_db
                .tx_loc_by_spent_output_loc_cf()
                .with_batch_for_writing(&mut batch)
                .zs_insert(&spent_output_location, &tx_loc);
            num_ops_in_write_batch += 1;
        }

        // Mark sprout, sapling and orchard nullifiers as spent
        for sprout_nullifier in tx.sprout_nullifiers() {
            batch.zs_insert(&sprout_nullifiers, sprout_nullifier, tx_loc);
            num_ops_in_write_batch += 1;
        }

        for sapling_nullifier in tx.sapling_nullifiers() {
            batch.zs_insert(&sapling_nullifiers, sapling_nullifier, tx_loc);
            num_ops_in_write_batch += 1;
        }

        for orchard_nullifier in tx.orchard_nullifiers() {
            batch.zs_insert(&orchard_nullifiers, orchard_nullifier, tx_loc);
            num_ops_in_write_batch += 1;
        }

        // Return before I/O calls if the upgrade is cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // write batches after processing all items in a transaction
        if num_ops_in_write_batch >= WRITE_BATCH_SIZE {
            db.write(std::mem::take(&mut batch))
                .expect("unexpected database write failure");
            num_ops_in_write_batch = 0;
        }
    }

    // write any remaining items in the batch to the db
    db.write(batch).expect("unexpected database write failure");

    Ok(())
}
