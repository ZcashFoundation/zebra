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

use crate::{service::finalized_state::disk_db::DiskDb, Config};

pub mod block;
pub mod chain;
pub mod metrics;
pub mod shielded;
pub mod transparent;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod arbitrary;

/// Wrapper struct to ensure high-level typed database access goes through the correct API.
#[derive(Clone, Debug)]
pub struct ZebraDb {
    /// The inner low-level database wrapper for the RocksDB database.
    /// This wrapper can be cloned and shared.
    db: DiskDb,
}

impl ZebraDb {
    /// Opens or creates the database at `config.path` for `network`,
    /// and returns a shared high-level typed database wrapper.
    pub fn new(config: &Config, network: Network) -> ZebraDb {
        ZebraDb {
            db: DiskDb::new(config, network),
        }
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
        self.db.shutdown(force);
    }
}
