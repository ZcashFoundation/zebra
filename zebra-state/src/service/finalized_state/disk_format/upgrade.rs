//! In-place format upgrades for the Zebra state database.

use std::cmp::Ordering;

use semver::Version;

use zebra_chain::parameters::Network;
use DbFormatChange::*;

use crate::{config::write_database_format_version_to_disk, Config};

/// The kind of database format change we're performing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DbFormatChange {
    /// Upgrading the format from `disk_version` to `running_version`.
    ///
    /// Until this upgrade is complete, the format is a mixture of both versions.
    Upgrade {
        disk_version: Version,
        running_version: Version,
    },

    /// Marking the format as downgraded from `disk_version` to `running_version`.
    ///
    /// Until the state is upgraded to `disk_version` by a later Zebra version,
    /// the format will be a mixture of both versions.
    Downgrade {
        disk_version: Version,
        running_version: Version,
    },
}

impl DbFormatChange {
    /// Check if loading `disk_version` into `running_version` needs a format change,
    /// and if it does, return the required format change.
    ///
    /// Also logs the kind of change at info level.
    ///
    /// If `disk_version` is `None`, Zebra is creating a new database.
    pub fn new(running_version: Version, disk_version: Option<Version>) -> Option<Self> {
        let Some(disk_version) = disk_version else {
            info!(
                ?running_version,
                "creating new database with the current format"
            );

            return None;
        };

        match disk_version.cmp(&running_version) {
            Ordering::Less => {
                info!(
                    ?running_version,
                    ?disk_version,
                    "trying to open older database format: launching upgrade task"
                );

                Some(Upgrade {
                    disk_version,
                    running_version,
                })
            }
            Ordering::Greater => {
                info!(
                    ?running_version,
                    ?disk_version,
                    "trying to open newer database format: data should be compatible"
                );

                Some(Downgrade {
                    disk_version,
                    running_version,
                })
            }
            Ordering::Equal => {
                info!(
                    ?running_version,
                    "trying to open compatible database format"
                );

                None
            }
        }
    }

    /// Returns true if this change is an upgrade.
    pub fn is_upgrade(&self) -> bool {
        matches!(self, Upgrade { .. })
    }

    /// Apply this format change to the database.
    /// Format changes should be launched in an independent `std::thread`.
    //
    // New format changes must be added to the *end* of this method.
    pub fn apply_format_change(&self, config: &Config, network: Network) {
        if !self.is_upgrade() {
            // # Correctness
            //
            // At the start of a format downgrade, the database must be marked as partially or
            // fully downgraded.
            Self::mark_as_changed(config, network);

            // Older supported versions just assume they can read newer formats,
            // because they can't predict all changes a newer Zebra version could make.
            //
            // The resposibility of staying backwards-compatible is on the newer version.
            // We do this on a best-effort basis for versions that are still supported.
            return;
        }

        // # TODO: link to format upgrade instructions doc here

        // # Correctness
        //
        // New code goes above this comment!
        //
        // Run the latest format upgrade code after the other upgrades are complete,
        // but before marking the format as upgraded.

        // At the end of a format upgrade, the database is marked as fully upgraded.
        // Upgrades can be run more than once if Zebra is restarted, so this is just a performance
        // optimisation.
        Self::mark_as_changed(config, network);
    }

    /// Mark the database as fully upgraded.
    /// This should be called after database format is up-to-date.
    ///
    /// # Concurrency
    ///
    /// The version must only be updated while RocksDB is holding the database
    /// directory lock. This prevents multiple Zebra instances corrupting the version
    /// file.
    fn mark_as_changed(config: &Config, network: Network) {
        write_database_format_version_to_disk(config, network)
            .expect("unable to write database format version file to disk");
    }
}
