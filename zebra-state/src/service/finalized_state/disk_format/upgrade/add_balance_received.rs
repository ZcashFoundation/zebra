//! Upgrades the database format for the column family tracking funds by address to include information
//! about funds received in addition to address balances.

use std::collections::HashSet;

use crossbeam_channel::{Receiver, TryRecvError};
use rayon::iter::{ParallelBridge, ParallelIterator};
use semver::Version;

use zebra_chain::{block::Height, transaction::Transaction};

use super::{CancelFormatChange, DiskFormatUpgrade};
use crate::{
    service::finalized_state::{disk_format::transparent::AddressBalanceLocationChange, ZebraDb},
    DiskWriteBatch, FromDisk, TransactionLocation, WriteDisk,
};

/// How many blocks to read transactions from per chunk when migrating the db format to add
/// received amounts by transparent address.
///
/// This limits the number of entries that will be stored in memory before writing to disk.
const NUM_BLOCKS_PER_CHUNK: usize = 200;

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
        // TODO:
        // - Mark upgrade progress in the database so we don't count the same outputs twice.
        // - Merge this upgrade with the value pools / block info one, get ^ for free.

        let network = db.network();
        let balance_by_transparent_addr = db.address_balance_cf();
        // TODO: Move 500k to constant if keeping this.
        let (output_sender, output_receiver) = std::sync::mpsc::sync_channel(500_000);

        let tx_loc_range =
            TransactionLocation::MIN..=TransactionLocation::max_for_height(initial_tip_height);

        // Return early before reading from disk if the upgrade was cancelled.
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        let db2 = db.clone();
        rayon::spawn_fifo(move || {
            db2.raw_transactions_by_location_range(tx_loc_range)
                // Parse and process transactions and outputs in parallel
                .par_bridge()
                // Deserialize each transaction and iterate over its transparent outputs
                .flat_map(|(_tx_loc, tx_bytes)| {
                    Transaction::from_bytes(tx_bytes.raw_bytes())
                        .outputs()
                        .to_vec()
                })
                .for_each(|output| {
                    if let Some(address) = output.address(&network) {
                        output_sender
                            .send((address, output.value().into()))
                            .expect("must send output to channel");
                    }
                });
        });

        while let Ok((address, value)) = output_receiver.recv() {
            // Return early before reading from disk if the upgrade was cancelled.
            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            // Update the value on disk.
            let mut batch = DiskWriteBatch::new();
            batch.zs_merge(
                balance_by_transparent_addr,
                address,
                AddressBalanceLocationChange::new_from_received(value),
            );
            db.write_batch(batch).expect("should write batch");
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
        let start_height = tip_height
            .as_usize()
            .checked_sub(NUM_BLOCKS_PER_CHUNK)
            .map_or(Height::MIN, |h| {
                Height(h.try_into().expect("tip height should fit"))
            });
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
}
