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
        Version::new(29, 2, 0)
    }

    fn description(&self) -> &'static str {
        "compact the address UTXO index"
    }

    // The address UTXO index is retained when block bodies are pruned.
    fn requires_full_history(&self) -> bool {
        false
    }

    // Compaction rewrites files, not records, so older readers can still open the database.
    fn changes_record_format(&self) -> bool {
        false
    }

    fn run(
        &self,
        _initial_tip_height: Height,
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        let errors_before = db.background_error_count();

        // About a minute on Mainnet in total; RocksDB serves reads and writes while it runs.
        // Chunks keyed by the first byte let shutdown cancel it between them.
        for (from, to) in compaction_ranges() {
            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            db.compact_address_utxo_index_range(Some(&from), to.as_ref().map(|to| to.as_slice()));
        }

        // A failed compaction only shows up as a background error. Leave the format version
        // unchanged so the next start runs the compaction again.
        if db.background_error_count() > errors_before {
            tracing::warn!("compacting the address UTXO index failed, retrying on the next start");
            return Err(CancelFormatChange);
        }

        Ok(())
    }
}

/// The 256 ranges `[byte, byte + 1)` covering every key, the last one open-ended.
///
/// Keep the end bound explicit: with `checked_add(1)` inside the loop, release builds never left it.
fn compaction_ranges() -> impl Iterator<Item = ([u8; 1], Option<[u8; 1]>)> {
    (0..=u8::MAX).map(|first_byte| {
        let to = if first_byte == u8::MAX {
            None
        } else {
            Some([first_byte.wrapping_add(1)])
        };
        ([first_byte], to)
    })
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use zebra_chain::parameters::Network;

    use crate::{service::finalized_state::FinalizedState, Config};

    use super::*;

    #[test]
    fn compaction_ranges_cover_every_first_byte_once() {
        let ranges: Vec<_> = compaction_ranges().collect();

        assert_eq!(ranges.len(), 256);
        assert_eq!(ranges[0], ([0], Some([1])));
        assert_eq!(ranges[254], ([254], Some([255])));
        assert_eq!(ranges[255], ([255], None));
    }

    #[test]
    fn compaction_finishes_on_an_ephemeral_database() {
        let _init_guard = zebra_test::init();

        let state = FinalizedState::new(&Config::ephemeral(), &Network::Mainnet)
            .expect("opening an ephemeral database should succeed");
        let db = state.db.clone();
        let (_cancel_sender, cancel_receiver) = crossbeam_channel::bounded(1);

        // Run on a thread so a loop that never ends fails the test instead of hanging it.
        let (done_sender, done_receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = Upgrade.run(Height(0), &db, &cancel_receiver);
            let _ = done_sender.send(result);
        });

        let result = done_receiver
            .recv_timeout(Duration::from_secs(60))
            .expect("the compaction should finish");
        assert_eq!(result, Ok(()));
    }
}
