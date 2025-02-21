//! Upgrades the database format for the column family tracking funds by address to include information
//! about funds received in addition to address balances.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use crossbeam_channel::{Receiver, TryRecvError};
use rayon::iter::{IntoParallelRefIterator, ParallelBridge, ParallelIterator};
use semver::Version;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::Height,
};

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
        if initial_tip_height.is_min() {
            return Ok(());
        }

        let network = &db.network();
        let balance_by_transparent_addr = db.cf_handle("balance_by_transparent_addr").unwrap();
        let initial_tx_loc_range = ..=TransactionLocation::max_for_height(initial_tip_height);

        let make_modifier = |received: Amount<NonNegative>| {
            move |received_acc: &mut Amount<NonNegative>| {
                *received_acc = (*received_acc + received).expect("should be valid");
            }
        };

        let id = || HashMap::new();
        let mut address_received_map = db
            .transactions_by_location_range(initial_tx_loc_range)
            .par_bridge()
            .flat_map(|(_tx_loc, tx)| tx.outputs().to_vec())
            .try_fold(id, |mut acc, output| {
                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    Err(CancelFormatChange)
                } else {
                    acc.entry(output.address(network).expect("should be valid"))
                        .and_modify(make_modifier(output.value))
                        .or_insert(output.value);

                    Ok(acc)
                }
            })
            .try_reduce(id, |mut acc, next_balance_map| {
                for (addr, balance) in next_balance_map {
                    acc.entry(addr)
                        .and_modify(make_modifier(balance))
                        .or_insert(balance);
                }

                Ok(acc)
            })?;

        let mut last_tip_height = initial_tip_height;

        'updater: loop {
            let batch = loop {
                let address_balances = loop {
                    let start_height = last_tip_height.next().expect("should be valid");
                    let tip_height = db.finalized_tip_height().expect("state has blocks");
                    last_tip_height = tip_height;

                    let start_bound = TransactionLocation::min_for_height(start_height);
                    let tx_loc_range =
                        start_bound..=TransactionLocation::max_for_height(tip_height);
                    for output in db
                        .transactions_by_location_range(tx_loc_range)
                        .flat_map(|(_, tx)| tx.outputs().to_vec())
                    {
                        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                            return Err(CancelFormatChange);
                        } else {
                            address_received_map
                                .entry(output.address(network).expect("should be valid"))
                                .and_modify(make_modifier(output.value))
                                .or_insert(output.value);
                        }
                    }

                    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                        return Err(CancelFormatChange);
                    }

                    if tip_height != db.finalized_tip_height().expect("state has blocks") {
                        continue;
                    }

                    let address_balances = address_received_map
                        .par_iter()
                        .map(|(address, &received)| {
                            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                                return Err(CancelFormatChange);
                            }

                            let mut balance = db
                                .address_balance_location(address)
                                .expect("should have address balances in finalized state");

                            *balance.received_mut() = received;

                            Ok((address, balance))
                        })
                        .collect::<Result<HashMap<_, _>, CancelFormatChange>>()?;

                    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                        return Err(CancelFormatChange);
                    }

                    if tip_height == db.finalized_tip_height().expect("state has blocks") {
                        break address_balances;
                    }
                };

                let mut batch = DiskWriteBatch::new();

                for (addr, balance) in &address_balances {
                    batch.zs_insert(balance_by_transparent_addr, addr, balance);
                }

                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                if last_tip_height == db.finalized_tip_height().expect("state has blocks") {
                    break batch;
                }
            };

            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            db.write_batch(batch).expect("should write batch");

            'checker: loop {
                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                // The write block task may prepare to write the next block and read the address balances before
                // these updates have been written, then overwrite the updated received balances.

                while last_tip_height == db.finalized_tip_height().expect("state has blocks") {
                    if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                        return Err(CancelFormatChange);
                    }

                    std::thread::sleep(Duration::from_millis(500));
                }

                let start_height = last_tip_height.next().expect("should be valid");
                let tip_height = db.finalized_tip_height().expect("state has blocks");
                last_tip_height = tip_height;

                let start_bound = TransactionLocation::min_for_height(start_height);
                let tx_loc_range = start_bound..=TransactionLocation::max_for_height(tip_height);
                for (_, tx) in db.transactions_by_location_range(tx_loc_range) {
                    for output in tx.outputs() {
                        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                            return Err(CancelFormatChange);
                        } else {
                            address_received_map
                                .entry(output.address(network).expect("should be valid"))
                                .and_modify(make_modifier(output.value))
                                .or_insert(output.value);
                        }
                    }
                }

                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                if last_tip_height != db.finalized_tip_height().expect("state has blocks") {
                    continue;
                }

                let is_updated_on_disk =
                    address_received_map.par_iter().all(|(address, &received)| {
                        // short-circuit iteration and immediately return an error in the next iteration of the outer loop
                        matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty))
                            && db
                                .address_balance_location(address)
                                .expect("should have address balances in finalized state")
                                .received()
                                == received
                    });

                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                if last_tip_height != db.finalized_tip_height().expect("state has blocks") {
                    continue;
                }

                if is_updated_on_disk {
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
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

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

        let addresses: HashSet<_> = db
            .transactions_by_location_range(tx_loc_range)
            .flat_map(|(_, tx)| tx.outputs().to_vec())
            .map(|output| output.address(network).expect("should be valid"))
            .collect();

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
