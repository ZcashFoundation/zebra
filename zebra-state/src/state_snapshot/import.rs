//! Importing a verified state snapshot into a new finalized database.
//!
//! Each verified piece becomes one SST file for its component. The importer
//! installs a database only after every consistency and chain check passes.

use std::path::{Path, PathBuf};

use crate::Config;

use super::format::ComponentId;

/// The directory under the state cache directory that holds import temporary
/// roots, one per bootstrap: `<cache_dir>/state-snapshot-import/<nonce>/`.
pub const IMPORT_DIR: &str = "state-snapshot-import";

/// Returns the directory that holds the import temporary roots for the state
/// `config`. A leftover from an interrupted bootstrap is deleted before the
/// next one starts.
pub fn import_dir(config: &Config) -> PathBuf {
    config.cache_dir.join(IMPORT_DIR)
}

/// Returns where the SST file built from piece `index` of `component` goes
/// under the import temporary root `temp_root`.
pub fn sst_path(temp_root: &Path, component: ComponentId, index: u32) -> PathBuf {
    temp_root
        .join("sst")
        .join(component.column_family())
        .join(format!("{index:08}.sst"))
}
