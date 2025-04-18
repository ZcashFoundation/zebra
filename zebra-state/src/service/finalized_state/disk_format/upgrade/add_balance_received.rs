//! Upgrades the database format for the column family tracking funds by address to include information
//! about funds received in addition to address balances.

use std::collections::{HashMap, HashSet};

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
        let address_received_map = db
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
                } else if let Some(address) = output.address(network) {
                    // Update the received balance of the address that received this output in the accumulating map of received balances.
                    acc.entry(address)
                        // Calls `make_modifier` to make a closure that adds the output value to the existing entry.
                        .and_modify(make_modifier(output.value.into()))
                        // Or inserts the output value as the received balance for the address if no entry exists.
                        .or_insert(output.value.into());

                    Ok(acc)
                } else {
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

        // Return early before the next disk read if the upgrade was cancelled and Zebra is shutting down.
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // Update the address balances until caught up to the finalized tip and prepare a disk write batch.
        let batch = {
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

            // Prepare a new disk write batch to update the address balances on-disk with the latest received balances.
            let mut batch = DiskWriteBatch::new();

            // Insert each updated address balances into the write batch.
            for (addr, balance) in &address_balances {
                batch.zs_insert(balance_by_transparent_addr, addr, balance);
            }

            batch
        };

        // Return early before writing the batch if the upgrade was cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // Write the batch to disk.
        db.write_batch(batch).expect("should write batch");

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
            .filter_map(|output| output.address(network))
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

    fn should_freeze_block_commits(&self) -> bool {
        true
    }
}
