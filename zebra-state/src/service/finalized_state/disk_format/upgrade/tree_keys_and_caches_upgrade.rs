//! Applies the [`fix_tree_key_type`] and [`cache_genesis_roots`] upgrades to the database.

use crossbeam_channel::Receiver;

use semver::Version;
use zebra_chain::block::Height;

use crate::service::finalized_state::ZebraDb;

use super::{cache_genesis_roots, fix_tree_key_type, CancelFormatChange, DiskFormatUpgrade};

/// Implements [`DiskFormatUpgrade`] for updating the sprout and history tree key type from
/// `Height` to the empty key `()` and the genesis note commitment trees to cache their roots
pub struct FixTreeKeyTypeAndCacheGenesisRoots;

impl DiskFormatUpgrade for FixTreeKeyTypeAndCacheGenesisRoots {
    fn version(&self) -> Version {
        Version::new(25, 3, 0)
    }

    fn description(&self) -> &'static str {
        "tree keys and caches upgrade"
    }

    #[allow(clippy::unwrap_in_result)]
    fn run(
        &self,
        initial_tip_height: Height,
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        // It shouldn't matter what order these are run in.
        cache_genesis_roots::run(initial_tip_height, db, cancel_receiver)?;
        fix_tree_key_type::run(initial_tip_height, db, cancel_receiver)?;
        Ok(())
    }

    #[allow(clippy::unwrap_in_result)]
    fn validate(
        &self,
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<Result<(), String>, CancelFormatChange> {
        let results = [
            cache_genesis_roots::detailed_check(db, cancel_receiver)?,
            fix_tree_key_type::detailed_check(db, cancel_receiver)?,
        ];

        let result = if results.iter().any(Result::is_err) {
            Err(format!("{results:?}"))
        } else {
            Ok(())
        };

        Ok(result)
    }
}
