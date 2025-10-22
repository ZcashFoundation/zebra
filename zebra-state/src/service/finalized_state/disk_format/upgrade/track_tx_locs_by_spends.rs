//! Tracks transaction locations by their inputs and revealed nullifiers.

use std::sync::Arc;

use crossbeam_channel::{Receiver, TryRecvError};
use rayon::iter::{IntoParallelIterator, ParallelIterator};

use zebra_chain::block::Height;

use crate::{
    service::{finalized_state::ZebraDb, non_finalized_state::Chain, read},
    Spend,
};

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

    (0..=initial_tip_height.0)
        .into_par_iter()
        .try_for_each(|height| {
            let height = Height(height);
            let mut batch = DiskWriteBatch::new();
            let mut should_index_at_height = false;

            let transactions = zebra_db.transactions_by_location_range(
                crate::TransactionLocation::from_index(height, 1)
                    ..=crate::TransactionLocation::max_for_height(height),
            );

            for (tx_loc, tx) in transactions {
                if tx.is_coinbase() {
                    continue;
                }

                if !should_index_at_height {
                    if let Some(spend) = tx
                        .inputs()
                        .iter()
                        .filter_map(|input| Some(input.outpoint()?.into()))
                        .chain(tx.sprout_nullifiers().cloned().map(Spend::from))
                        .chain(tx.sapling_nullifiers().cloned().map(Spend::from))
                        .chain(tx.orchard_nullifiers().cloned().map(Spend::from))
                        .next()
                    {
                        if read::spending_transaction_hash::<Arc<Chain>>(None, zebra_db, spend)
                            .is_some()
                        {
                            // Skip transactions in blocks with existing indexes
                            return Ok(());
                        } else {
                            should_index_at_height = true
                        }
                    } else {
                        continue;
                    };
                }

                for input in tx.inputs() {
                    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
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

                batch.prepare_nullifier_batch(zebra_db, &tx, tx_loc);
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
