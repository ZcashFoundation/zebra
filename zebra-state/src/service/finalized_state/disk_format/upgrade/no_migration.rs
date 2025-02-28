//! An implementation of [`DiskFormatUpgrade`] for marking the database as upgraded to a new format version.

use crossbeam_channel::Receiver;

use semver::Version;
use zebra_chain::block::Height;

use crate::service::finalized_state::ZebraDb;

use super::{CancelFormatChange, DiskFormatUpgrade};

/// Implements [`DiskFormatUpgrade`] for in-place upgrades that do not involve any migration
/// of existing data into the new format.
pub struct NoMigration {
    version: Version,
}

impl NoMigration {
    /// Creates a new instance of the [`NoMigration`] upgrade.
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            version: Version::new(major, minor, patch),
        }
    }
}

impl DiskFormatUpgrade for NoMigration {
    fn version(&self) -> Version {
        self.version.clone()
    }

    fn description(&self) -> &'static str {
        "no migration"
    }

    #[allow(clippy::unwrap_in_result)]
    fn run(
        &self,
        _initial_tip_height: Height,
        _db: &ZebraDb,
        _cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        Ok(())
    }

    fn needs_migration(&self) -> bool {
        false
    }
}
