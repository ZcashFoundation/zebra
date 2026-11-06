//! Cached state configuration for Zebra.

use std::{
    fs::{canonicalize, remove_dir_all, DirEntry, ReadDir},
    path::{Path, PathBuf},
};

use semver::Version;
use tokio::task::{spawn_blocking, JoinHandle};
use tracing::Span;

use zebra_chain::parameters::Network;

use crate::{
    constants::STATE_DATABASE_KIND,
    service::finalized_state::{restorable_db_source_versions, restorable_db_versions},
    state_database_format_version_in_code, BoxError,
};

pub use zebra_db::config::{
    database_format_version_at_path, database_format_version_on_disk, Config, RedactedString,
};

#[cfg(test)]
mod tests;

// Cleaning up old database versions
// TODO: put this in a different module?

/// Spawns a task that checks if there are old state database folders,
/// and deletes them from the filesystem.
///
/// See `check_and_delete_old_databases()` for details.
pub fn check_and_delete_old_state_databases(config: &Config, network: &Network) -> JoinHandle<()> {
    check_and_delete_old_databases(
        config,
        STATE_DATABASE_KIND,
        state_database_format_version_in_code().major,
        network,
    )
}

/// Spawns a task that checks if there are old database folders,
/// and deletes them from the filesystem.
///
/// Iterate over the files and directories in the databases folder and delete if:
/// - The `db_kind` directory exists.
/// - The entry in `db_kind` is a directory.
/// - The directory name has a prefix `v`.
/// - The directory name without the prefix can be parsed as an unsigned number.
/// - The parsed number is lower than the `major_version`.
///
/// The network is used to generate the path, then ignored.
/// If `config` is an ephemeral database, no databases are deleted.
///
/// # Panics
///
/// If the path doesn't match the expected `db_kind/major_version/network` format.
pub fn check_and_delete_old_databases(
    config: &Config,
    db_kind: impl AsRef<str>,
    major_version: u64,
    network: &Network,
) -> JoinHandle<()> {
    let current_span = Span::current();
    let config = config.clone();
    let db_kind = db_kind.as_ref().to_string();
    let network = network.clone();

    spawn_blocking(move || {
        current_span.in_scope(|| {
            delete_old_databases(config, db_kind, major_version, &network);
            info!("finished old database version cleanup task");
        })
    })
}

/// Check if there are old database folders and delete them from the filesystem.
///
/// See [`check_and_delete_old_databases`] for details.
fn delete_old_databases(config: Config, db_kind: String, major_version: u64, network: &Network) {
    if config.ephemeral || !config.delete_old_database {
        return;
    }

    info!(db_kind, "checking for old database versions");

    let restorable_db_versions = restorable_db_versions();

    let mut db_path = config.db_path(&db_kind, major_version, network);
    // Check and remove the network path.
    assert_eq!(
        db_path.file_name(),
        Some(network.directory_name().as_ref()),
        "unexpected database network path structure"
    );
    assert!(db_path.pop());

    // Check and remove the major version path, we'll iterate over them all below.
    assert_eq!(
        db_path.file_name(),
        Some(format!("v{major_version}").as_ref()),
        "unexpected database version path structure"
    );
    assert!(db_path.pop());

    // Check for the correct database kind to iterate within.
    assert_eq!(
        db_path.file_name(),
        Some(db_kind.as_ref()),
        "unexpected database kind path structure"
    );

    if let Some(db_kind_dir) = read_dir(&db_path) {
        for entry in db_kind_dir.flatten() {
            let deleted_db =
                check_and_delete_database(&config, major_version, &restorable_db_versions, &entry);

            if let Some(deleted_db) = deleted_db {
                info!(?deleted_db, "deleted outdated {db_kind} database directory");
            }
        }
    }
}

/// Return a `ReadDir` for `dir`, after checking that `dir` exists and can be read.
///
/// Returns `None` if any operation fails.
fn read_dir(dir: &Path) -> Option<ReadDir> {
    if dir.exists() {
        if let Ok(read_dir) = dir.read_dir() {
            return Some(read_dir);
        }
    }
    None
}

/// Check if `entry` is an old database directory, and delete it from the filesystem.
/// See [`check_and_delete_old_databases`] for details.
///
/// If the directory was deleted, returns its path.
fn check_and_delete_database(
    config: &Config,
    major_version: u64,
    restorable_db_versions: &[u64],
    entry: &DirEntry,
) -> Option<PathBuf> {
    let dir_name = parse_dir_name(entry)?;
    let dir_major_version = parse_major_version(&dir_name)?;

    if dir_major_version >= major_version {
        return None;
    }

    // Don't delete databases that can be reused.
    if restorable_db_versions
        .iter()
        .flat_map(|version| restorable_db_source_versions(*version))
        .any(|version| version == dir_major_version)
    {
        return None;
    }

    let outdated_path = entry.path();

    // # Correctness
    //
    // Check that the path we're about to delete is inside the cache directory.
    // If the user has symlinked the outdated state directory to a non-cache directory,
    // we don't want to delete it, because it might contain other files.
    // We don't attempt to guard against malicious symlinks created by attackers
    // (TOCTOU attacks). Zebra should not be run with elevated permissions.
    let cache_path = canonicalize(&config.cache_dir).ok()?;
    let outdated_path = canonicalize(outdated_path).ok()?;

    if !outdated_path.starts_with(&cache_path) {
        info!(
            skipped_path = ?outdated_path,
            ?cache_path,
            "skipped cleanup of outdated state directory: state is outside cache directory",
        );

        return None;
    }

    remove_dir_all(&outdated_path).ok().map(|()| outdated_path)
}

/// Check if `entry` is a directory with a valid UTF-8 name.
/// (State directory names are guaranteed to be UTF-8.)
///
/// Returns `None` if any operation fails.
fn parse_dir_name(entry: &DirEntry) -> Option<String> {
    if let Ok(file_type) = entry.file_type() {
        if file_type.is_dir() {
            if let Ok(dir_name) = entry.file_name().into_string() {
                return Some(dir_name);
            }
        }
    }
    None
}

/// Parse the database major version number from `dir_name`.
///
/// Returns `None` if parsing fails, or the directory name is not in the expected format.
fn parse_major_version(dir_name: &str) -> Option<u64> {
    dir_name
        .strip_prefix('v')
        .and_then(|version| version.parse().ok())
}

// TODO: move these to the format upgrade module

/// Returns the full semantic version of the on-disk state database, based on its config and network.
pub fn state_database_format_version_on_disk(
    config: &Config,
    network: &Network,
) -> Result<Option<Version>, BoxError> {
    database_format_version_on_disk(
        config,
        STATE_DATABASE_KIND,
        state_database_format_version_in_code().major,
        network,
    )
}

// Hide this destructive method from the public API, except in tests.
#[allow(unused_imports)]
pub(crate) use hidden::{
    write_database_format_version_at_path, write_database_format_version_to_disk,
    write_state_database_format_version_to_disk,
};

pub(crate) mod hidden {
    #![allow(dead_code)]

    pub use zebra_db::config::hidden::{
        write_database_format_version_at_path, write_database_format_version_to_disk,
    };

    use super::*;

    /// Writes `changed_version` to the on-disk state database after the format is changed.
    /// (Or a new database is created.)
    ///
    /// See `write_database_format_version_to_disk()` for details.
    pub fn write_state_database_format_version_to_disk(
        config: &Config,
        changed_version: &Version,
        network: &Network,
    ) -> Result<(), BoxError> {
        write_database_format_version_to_disk(
            config,
            STATE_DATABASE_KIND,
            state_database_format_version_in_code().major,
            changed_version,
            network,
        )
    }
}
