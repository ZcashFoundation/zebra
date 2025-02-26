//! Upgrades the database format for the column family tracking funds by address to include information
//! about funds received in addition to address balances.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use crossbeam_channel::{Receiver, TryRecvError};
use rayon::iter::{IntoParallelRefIterator, ParallelBridge, ParallelIterator};
use semver::Version;

use zebra_chain::block::Height;

use crate::{service::finalized_state::ZebraDb, DiskWriteBatch, TransactionLocation, WriteDisk};

use super::{CancelFormatChange, DiskFormatUpgrade};

/// Implements [`DiskFormatUpgrade`] for tracking funds received by address in the database.
pub struct AddAddressBalanceReceived;

impl DiskFormatUpgrade for AddAddressBalanceReceived {
    fn version(&self) -> Version {
        Version::new(26, 1, 0)
    }

    fn description(&self) -> &'static str {
        "add address received balances upgrade"
    }

    #[allow(clippy::unwrap_in_result)]
    fn run(
        &self,
        initial_tip_height: Height,
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        let network = &db.network();
        let balance_by_transparent_addr = db.cf_handle("balance_by_transparent_addr").unwrap();

        // Return early if there are no blocks in the state and the database is empty.
        // Note: The caller should (and as of this writing, does) return early if the database is empty anyway.
        if initial_tip_height.is_min() {
            return Ok(());
        }

        // Set the initial query range as all transaction locations up to the max transaction index for the initial tip height.
        let initial_tx_loc_range = ..=TransactionLocation::max_for_height(initial_tip_height);

        // A factory closure that constructs closures to add a provided value to the received balance of an address.
        let make_modifier = |received: u64| {
            move |received_acc: &mut u64| {
                *received_acc = received_acc.checked_add(received).unwrap_or(u64::MAX);
            }
        };

        // Read all of the transactions up to the initial tip height and tally the received balances by address.
        let id = || HashMap::new();
        let mut address_received_map = db
            .transactions_by_location_range(initial_tx_loc_range)
            // Process transactions and outputs in parallel
            .par_bridge()
            // Map each transaction to its outputs
            .flat_map(|(_tx_loc, tx)| tx.outputs().to_vec())
            // Process each output by adding its value to the received balance of the address that received it.
            .try_fold(id, |mut acc, output| {
                // Return an error to short-circuit the parallel iterator and return early
                // if the upgrade was cancelled and Zebra is shutting down.
                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    Err(CancelFormatChange)
                } else {
                    // Update the received balance of the address that received this output in the accumulating map of received balances.
                    acc.entry(output.address(network).expect("should be valid"))
                        // Calls `make_modifier` to make a closure that adds the output value to the existing entry.
                        .and_modify(make_modifier(output.value.into()))
                        // Or inserts the output value as the received balance for the address if no entry exists.
                        .or_insert(output.value.into());

                    Ok(acc)
                }
            })
            // Combine the collections of received balances for each address aggregated by each worker thread into a single map.
            .try_reduce(id, |mut acc, next_balance_map| {
                // Add the received balance for each address in the next map to that of the accumulated map.
                for (addr, balance) in next_balance_map {
                    acc.entry(addr)
                        .and_modify(make_modifier(balance))
                        .or_insert(balance);
                }

                Ok(acc)
            })?;

        // Set the last tip height that was processed and accounted for in the received balances.
        let mut last_tip_height = initial_tip_height;

        // Update the address balances with the received balances, check that they were updated correctly, and repeat until successful.
        'updater: loop {
            // Update the address balances until caught up to the finalized tip and prepare a disk write batch.
            let batch = loop {
                // Update the address received balances and read the address balances from disk until caught up to the finalized tip.
                let address_balances = loop {
                    // Set the start height of the query range as the next height after the last tip height that was processed.
                    let start_height = last_tip_height.next().expect("should be valid");
                    // Set the latest finalized tip height as the new last processed tip height.
                    last_tip_height = db.finalized_tip_height().expect("state has blocks");

                    // Process new outputs in blocks from the start height (last processed height) up to the latest finalized tip height.
                    let start_bound = TransactionLocation::min_for_height(start_height);
                    let tx_loc_range =
                        start_bound..=TransactionLocation::max_for_height(last_tip_height);
                    for output in db
                        .transactions_by_location_range(tx_loc_range)
                        .flat_map(|(_, tx)| tx.outputs().to_vec())
                    {
                        // Return early before the next disk read if the upgrade was cancelled and Zebra is shutting down.
                        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                            return Err(CancelFormatChange);
                        } else {
                            address_received_map
                                .entry(output.address(network).expect("should be valid"))
                                .and_modify(make_modifier(output.value.into()))
                                .or_insert(output.value.into());
                        }
                    }

                    // Return early before the next disk read if the upgrade was cancelled and Zebra is shutting down.
                    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                        return Err(CancelFormatChange);
                    }

                    // If the finalized tip has changed while processing the new outputs in blocks since the last tip height,
                    // continue to the next iteration of the `address_balances` loop to update the received balances by address map.
                    if last_tip_height != db.finalized_tip_height().expect("state has blocks") {
                        continue;
                    }

                    // Read the address balances from disk to an in-memory map of address balances with updated received balances.
                    let address_balances = address_received_map
                        // Read balances in parallel by address.
                        .par_iter()
                        .map(|(address, &received)| {
                            // Return an error to short-circuit the parallel iterator and return early
                            // if the upgrade was cancelled and Zebra is shutting down.
                            //
                            // Note: `.collect::<Result<_, _>>()?` will short-circuit a parallel iterator on the first error.
                            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                                return Err(CancelFormatChange);
                            }

                            // Read the address balance from disk.
                            let mut balance = db
                                .address_balance_location(address)
                                .expect("should have address balances in finalized state");

                            // Update the address balance with the tallied received balance.
                            *balance.received_mut() = received;

                            Ok((address, balance))
                        })
                        .collect::<Result<HashMap<_, _>, CancelFormatChange>>()?;

                    // Return early before the next disk read if the upgrade was cancelled and Zebra is shutting down.
                    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                        return Err(CancelFormatChange);
                    }

                    // If the latest finalized tip height matches the last processed height, break the loop with the map of address balances,
                    // otherwise, if the finalized tip height has changed while reading address balances from disk and updating their received balances,
                    // continue to the next iteration of the `address_balances` loop to update the in-memory address received balances and try again.
                    if last_tip_height == db.finalized_tip_height().expect("state has blocks") {
                        break address_balances;
                    }
                };

                // Prepare a new disk write batch to update the address balances on-disk with the latest received balances.
                let mut batch = DiskWriteBatch::new();

                // Insert each updated address balances into the write batch.
                for (addr, balance) in &address_balances {
                    batch.zs_insert(balance_by_transparent_addr, addr, balance);
                }

                // Return early before the next disk read if the upgrade was cancelled and Zebra is shutting down.
                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                // Check one last time that the finalized tip height hasn't changed before breaking the `batch` loop with
                // the prepare disk write batch.
                if last_tip_height == db.finalized_tip_height().expect("state has blocks") {
                    break batch;
                }
            };

            // Return early before writing the batch if the upgrade was cancelled.
            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            // Write the batch to disk.
            db.write_batch(batch).expect("should write batch");

            // The write block task may read the address balances to prepare to write the next block before
            // these updates were written, and then overwrite the updated received balances with partial values.
            //
            // To ensure that the address balances were updated correctly, check that the address balances were
            // updated correctly before breaking the `updater` loop.
            'checker: loop {
                // Return early before the next disk read if the upgrade was cancelled.
                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                // Wait until the block write task has had a chance to overwrite the changes written to disk
                // from here before checking the updated address balances.
                while last_tip_height == db.finalized_tip_height().expect("state has blocks") {
                    // Return early before the next disk read if the upgrade was cancelled.
                    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                        return Err(CancelFormatChange);
                    }

                    // Sleep for a short duration between checking the finalized tip height while waiting for
                    // the block write task to write the next block to avoid busy-waiting.
                    std::thread::sleep(Duration::from_millis(500));
                }

                // Read any new outputs in the finalized chain since the last tip height and update the received balances by address.

                // Set the start height of a new query range as the next height after the last tip height that was processed.
                let start_height = last_tip_height.next().expect("should be valid");
                // Set the new last tip height as the latest finalized tip height.
                last_tip_height = db.finalized_tip_height().expect("state has blocks");

                let start_bound = TransactionLocation::min_for_height(start_height);
                let tx_loc_range =
                    start_bound..=TransactionLocation::max_for_height(last_tip_height);
                for (_, tx) in db.transactions_by_location_range(tx_loc_range) {
                    for output in tx.outputs() {
                        // Return early before the next disk read if the upgrade was cancelled.
                        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                            return Err(CancelFormatChange);
                        } else {
                            // Update the received balance of the address that received this output in the map of received balances.
                            address_received_map
                                .entry(output.address(network).expect("should be valid"))
                                .and_modify(make_modifier(output.value.into()))
                                .or_insert(output.value.into());
                        }
                    }
                }

                // Return early before the next disk read if the upgrade was cancelled.
                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                // If the finalized tip has changed while processing the new outputs in blocks since the last tip height,
                // continue to the next iteration of the `checker` loop to keep updating the received balances by address map.
                if last_tip_height != db.finalized_tip_height().expect("state has blocks") {
                    continue 'checker;
                }

                // Check that all in-memory address balances have matching values on-disk (in parallel).
                // TODO: Retain items that don't have the right received balance instead?
                // Note: This would require ignoring outputs sent to addresses that are not being tracked
                //       after the first disk write.
                let is_updated_on_disk =
                    address_received_map.par_iter().all(|(address, &received)| {
                        // If the upgrade was cancelled, short-circuit iteration and return an error in the next iteration of
                        // the outer loop before the next disk read.
                        let is_not_cancelled =
                            matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty));
                        // Check that the address balance on disk matches the received balance in memory.
                        is_not_cancelled
                            && db
                                .address_balance_location(address)
                                .expect("should have address balances in finalized state")
                                .received()
                                == received
                    });

                if !is_updated_on_disk {
                    // The address balances were not updated correctly, try again.
                    warn!("address balances were not updated correctly, retrying");
                    break 'checker;
                }

                // Return early before the next disk read if the upgrade was cancelled.
                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                // Check one last time that the finalized tip height hasn't changed before breaking the `updater` loop and
                // finishing in-place migration for the add balance received format upgrade.
                if last_tip_height == db.finalized_tip_height().expect("state has blocks") {
                    break 'updater;
                } else {
                    // The finalized tip height hasn't changed but the data differs from what's expected, try again.
                    warn!("address balances were updated while writing, retrying");
                    break 'checker;
                }
            }
        }

        Ok(())
    }

    #[allow(clippy::unwrap_in_result)]
    fn validate(
        &self,
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<Result<(), String>, CancelFormatChange> {
        // Return early before the next disk read if the upgrade was cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // Read the finalized tip height or return early if the database is empty.
        let Some(tip_height) = db.finalized_tip_height() else {
            return Ok(Ok(()));
        };

        // Check any outputs in the last 1000 blocks
        let network = &db.network();
        let start_height = (tip_height - 1_000).unwrap_or(Height::MIN);
        let tx_loc_range = TransactionLocation::min_for_height(start_height)..;

        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // Collect the set of addresses that received transparent funds in the last query range (last 1000 blocks).
        let addresses: HashSet<_> = db
            .transactions_by_location_range(tx_loc_range)
            .flat_map(|(_, tx)| tx.outputs().to_vec())
            .map(|output| output.address(network).expect("should be valid"))
            .collect();

        // Check that no address balances for that set of addresses have a received field of `0`.
        for address in addresses {
            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            let balance = db
                .address_balance_location(&address)
                .expect("should have address balances in finalized state");

            if balance.received() == 0 {
                return Ok(Err(format!(
                    "unexpected balance received for address {}: {}",
                    address,
                    balance.received(),
                )));
            }
        }

        Ok(Ok(()))
    }
}
