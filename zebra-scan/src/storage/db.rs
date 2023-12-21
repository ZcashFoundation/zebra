//! Persistent storage for scanner results.

use std::path::Path;

use semver::Version;

use zebra_chain::parameters::Network;

use crate::Config;

use super::Storage;

// Public types and APIs
pub use zebra_state::{
    SaplingScannedDatabaseEntry, SaplingScannedDatabaseIndex, SaplingScannedResult,
    SaplingScanningKey, ZebraDb as ScannerDb,
};

pub mod sapling;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod tests;

/// The directory name used to distinguish the scanner database from Zebra's other databases or
/// flat files.
///
/// We use "private" in the name to warn users not to share this data.
pub const SCANNER_DATABASE_KIND: &str = "private-scan";

/// The column families supported by the running `zebra-scan` database code.
///
/// Existing column families that aren't listed here are preserved when the database is opened.
pub const SCANNER_COLUMN_FAMILIES_IN_CODE: &[&str] = &[
    // Sapling
    sapling::SAPLING_TX_IDS,
    // Orchard
    // TODO: add Orchard support
];

/// The major version number of the scanner database. This must be updated whenever the database
/// format changes.
const SCANNER_DATABASE_FORMAT_MAJOR_VERSION: u64 = 1;

impl Storage {
    // Creation

    /// Opens and returns an on-disk scanner results database instance for `config` and `network`.
    /// If there is no existing database, creates a new database on disk.
    ///
    /// New keys in `config` are not inserted into the database.
    pub(crate) fn new_db(config: &Config, network: Network, read_only: bool) -> Self {
        Self::new_with_debug(
            config, network,
            // TODO: make format upgrades work with any database, then change debug_skip_format_upgrades to `false`
            true, read_only,
        )
    }

    /// Returns an on-disk database instance with the supplied production and debug settings.
    /// If there is no existing database, creates a new database on disk.
    ///
    /// New keys in `config` are not inserted into the database.
    ///
    /// This method is intended for use in tests.
    pub(crate) fn new_with_debug(
        config: &Config,
        network: Network,
        debug_skip_format_upgrades: bool,
        read_only: bool,
    ) -> Self {
        let db = ScannerDb::new(
            config.db_config(),
            SCANNER_DATABASE_KIND,
            &Self::database_format_version_in_code(),
            network,
            debug_skip_format_upgrades,
            SCANNER_COLUMN_FAMILIES_IN_CODE
                .iter()
                .map(ToString::to_string),
            read_only,
        );

        let new_storage = Self { db };

        // Report where we are for each key in the database.
        let keys = new_storage.sapling_keys_last_heights();
        for (key_num, (_key, height)) in keys.iter().enumerate() {
            info!(
                "Last scanned height for key number {} is {}, resuming at {}",
                key_num,
                height.as_usize(),
                height.next().expect("height is not maximum").as_usize(),
            );
        }

        info!("loaded Zebra scanner cache");

        new_storage
    }

    // Config

    /// Returns the configured network for this database.
    pub fn network(&self) -> Network {
        self.db.network()
    }

    /// Returns the `Path` where the files used by this database are located.
    pub fn path(&self) -> &Path {
        self.db.path()
    }

    // Versioning & Upgrades

    /// The database format version in the running scanner code.
    pub fn database_format_version_in_code() -> Version {
        // TODO: implement in-place scanner database format upgrades
        Version::new(SCANNER_DATABASE_FORMAT_MAJOR_VERSION, 0, 0)
    }

    /// Check for panics in code running in spawned threads.
    /// If a thread exited with a panic, resume that panic.
    ///
    /// This method should be called regularly, so that panics are detected as soon as possible.
    //
    // TODO: when we implement format changes, call this method regularly
    pub fn check_for_panics(&mut self) {
        self.db.check_for_panics()
    }

    // General database status

    /// Returns true if the database is empty.
    pub fn is_empty(&self) -> bool {
        // Any column family that is populated at (or near) startup can be used here.
        self.sapling_tx_ids_cf().zs_is_empty()
    }
}
