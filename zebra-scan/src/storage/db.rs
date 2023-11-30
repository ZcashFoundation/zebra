//! Persistent storage for scanner results.

use std::{collections::HashMap, path::Path};

use semver::Version;

use zebra_chain::parameters::Network;

use crate::Config;

use super::Storage;

// Public types and APIs
pub use zebra_state::ZebraDb as ScannerDb;

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
    "sapling_tx_ids",
    // Orchard
    // TODO
];

impl Storage {
    /// Opens and returns an on-disk scanner results database instance for `config` and `network`.
    /// If there is no existing database, creates a new database on disk.
    ///
    /// New keys in `config` are not inserted into the database.
    pub(crate) fn new_db(config: &Config, network: Network) -> Self {
        Self::new_with_debug(
            config, network,
            // TODO: make format upgrades work with any database, then change this to `false`
            true,
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
        );

        let new_storage = Self {
            db,
            sapling_keys: HashMap::new(),
            sapling_results: HashMap::new(),
        };

        // TODO: report the last scanned height here?
        tracing::info!("loaded Zebra scanner cache");

        new_storage
    }

    /// The database format version in the running scanner code.
    pub fn database_format_version_in_code() -> Version {
        // TODO: implement scanner database versioning
        Version::new(0, 0, 0)
    }

    /// Returns the configured network for this database.
    pub fn network(&self) -> Network {
        self.db.network()
    }

    /// Returns the `Path` where the files used by this database are located.
    pub fn path(&self) -> &Path {
        self.db.path()
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
}
