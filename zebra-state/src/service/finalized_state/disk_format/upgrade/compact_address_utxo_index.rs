//! Compacts the address UTXO index once, so address queries stop skipping old deletion markers.

use crossbeam_channel::{Receiver, TryRecvError};
use semver::Version;
use zebra_chain::block::Height;

use crate::service::finalized_state::ZebraDb;

use super::{CancelFormatChange, DiskFormatUpgrade};

/// Compacts `utxo_loc_by_transparent_addr_loc`.
///
/// Databases written before the deletion-triggered compaction was enabled can hold millions of
/// deletion markers for addresses whose outputs were all spent, such as the funding stream
/// addresses, which makes their address queries slow until RocksDB compacts those files.
pub struct Upgrade;

impl DiskFormatUpgrade for Upgrade {
    fn version(&self) -> Version {
        Version::new(29, 1, 0)
    }

    fn description(&self) -> &'static str {
        "compact the address UTXO index"
    }

    fn run(
        &self,
        _initial_tip_height: Height,
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }

        // About a minute on Mainnet; RocksDB serves reads and writes while it runs.
        db.compact_address_utxo_index();

        Ok(())
    }
}
