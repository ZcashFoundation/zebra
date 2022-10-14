//! Provides high-level access to the database using [`zebra_chain`] types.
//!
//! This module makes sure that:
//! - all disk writes happen inside a RocksDB transaction, and
//! - format-specific invariants are maintained.
//!
//! # Correctness
//!
//! The [`crate::constants::DATABASE_FORMAT_VERSION`] constant must
//! be incremented each time the database format (column, serialization, etc) changes.

use std::path::Path;

use zebra_chain::parameters::Network;

use crate::{
    service::finalized_state::{disk_db::DiskDb, disk_format::block::MAX_ON_DISK_HEIGHT},
    Config,
};

pub mod block;
pub mod chain;
pub mod metrics;
pub mod shielded;
pub mod transparent;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;

/// Wrapper struct to ensure high-level typed database access goes through the correct API.
///
/// `rocksdb` allows concurrent writes through a shared reference,
/// so database instances are cloneable. When the final clone is dropped,
/// the database is closed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZebraDb {
    // Owned State
    //
    // Everything contained in this state must be shared by all clones, or read-only.
    //
    /// The inner low-level database wrapper for the RocksDB database.
    db: DiskDb,
}

impl ZebraDb {
    /// Opens or creates the database at `config.path` for `network`,
    /// and returns a shared high-level typed database wrapper.
    pub fn new(config: &Config, network: Network) -> ZebraDb {
        let db = ZebraDb {
            db: DiskDb::new(config, network),
        };

        db.check_max_on_disk_tip_height();

        db
    }

    /// Returns the `Path` where the files used by this database are located.
    pub fn path(&self) -> &Path {
        self.db.path()
    }

    /// Shut down the database, cleaning up background tasks and ephemeral data.
    ///
    /// If `force` is true, clean up regardless of any shared references.
    /// `force` can cause errors accessing the database from other shared references.
    /// It should only be used in debugging or test code, immediately before a manual shutdown.
    ///
    /// See [`DiskDb::shutdown`] for details.
    pub(crate) fn shutdown(&mut self, force: bool) {
        self.check_max_on_disk_tip_height();

        self.db.shutdown(force);
    }

    /// Check that the on-disk height is well below the maximum supported database height.
    ///
    /// Zebra only supports on-disk heights up to 3 bytes.
    ///
    /// # Logs an Error
    ///
    /// If Zebra is storing block heights that are close to [`MAX_ON_DISK_HEIGHT`].
    fn check_max_on_disk_tip_height(&self) {
        if let Some((tip_height, tip_hash)) = self.tip() {
            if tip_height.0 > MAX_ON_DISK_HEIGHT.0 / 2 {
                error!(
                    ?tip_height,
                    ?tip_hash,
                    ?MAX_ON_DISK_HEIGHT,
                    "unexpectedly large tip height, database format upgrade required",
                );
            }
        }
    }
}
