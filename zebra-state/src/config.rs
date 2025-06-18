//! Cached state configuration for Zebra.

use std::{
    fs::{self, canonicalize, remove_dir_all, DirEntry, ReadDir},
    io::ErrorKind,
    path::{Path, PathBuf},
    time::Duration,
};

use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::task::{spawn_blocking, JoinHandle};
use tracing::Span;

use zebra_chain::{common::default_cache_dir, parameters::Network};

use crate::{
    constants::{DATABASE_FORMAT_VERSION_FILE_NAME, STATE_DATABASE_KIND},
    service::finalized_state::restorable_db_versions,
    state_database_format_version_in_code, BoxError,
};

/// Configuration for the state service.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The root directory for storing cached block data.
    ///
    /// If you change this directory, you might also want to change `network.cache_dir`.
    ///
    /// This cache stores permanent blockchain state that can be replicated from
    /// the network, including the best chain, blocks, the UTXO set, and other indexes.
    /// Any state that can be rolled back is only stored in memory.
    ///
    /// The `zebra-state` cache does *not* include any private data, such as wallet data.
    ///
    /// You can delete the entire cached state directory, but it will impact your node's
    /// readiness and network usage. If you do, Zebra will re-sync from genesis the next
    /// time it is launched.
    ///
    /// The default directory is platform dependent, based on
    /// [`dirs::cache_dir()`](https://docs.rs/dirs/3.0.1/dirs/fn.cache_dir.html):
    ///
    /// |Platform | Value                                           | Example                              |
    /// | ------- | ----------------------------------------------- | ------------------------------------ |
    /// | Linux   | `$XDG_CACHE_HOME/zebra` or `$HOME/.cache/zebra` | `/home/alice/.cache/zebra`           |
    /// | macOS   | `$HOME/Library/Caches/zebra`                    | `/Users/Alice/Library/Caches/zebra`  |
    /// | Windows | `{FOLDERID_LocalAppData}\zebra`                 | `C:\Users\Alice\AppData\Local\zebra` |
    /// | Other   | `std::env::current_dir()/cache/zebra`           | `/cache/zebra`                       |
    ///
    /// # Security
    ///
    /// If you are running Zebra with elevated permissions ("root"), create the
    /// directory for this file before running Zebra, and make sure the Zebra user
    /// account has exclusive access to that directory, and other users can't modify
    /// its parent directories.
    ///
    /// # Implementation Details
    ///
    /// Each state format version and network has a separate state.
    /// These states are stored in `state/vN/mainnet` and `state/vN/testnet` subdirectories,
    /// underneath the `cache_dir` path, where `N` is the state format version.
    ///
    /// When Zebra's state format changes, it creates a new state subdirectory for that version,
    /// and re-syncs from genesis.
    ///
    /// Old state versions are automatically deleted at startup. You can also manually delete old
    /// state versions.
    pub cache_dir: PathBuf,

    /// Whether to use an ephemeral database.
    ///
    /// Ephemeral databases are stored in a temporary directory created using [`tempfile::tempdir()`].
    /// They are deleted when Zebra exits successfully.
    /// (If Zebra panics or crashes, the ephemeral database won't be deleted.)
    ///
    /// Set to `false` by default. If this is set to `true`, [`cache_dir`] is ignored.
    ///
    /// Ephemeral directories are created in the [`std::env::temp_dir()`].
    /// Zebra names each directory after the state version and network, for example: `zebra-state-v21-mainnet-XnyGnE`.
    ///
    /// [`cache_dir`]: struct.Config.html#structfield.cache_dir
    pub ephemeral: bool,

    /// Whether to delete the old database directories when present.
    ///
    /// Set to `true` by default. If this is set to `false`,
    /// no check for old database versions will be made and nothing will be
    /// deleted.
    pub delete_old_database: bool,

    // Debug configs
    //
    /// Commit blocks to the finalized state up to this height, then exit Zebra.
    ///
    /// Set to `None` by default: Zebra continues syncing indefinitely.
    pub debug_stop_at_height: Option<u32>,

    /// While Zebra is running, check state validity this often.
    ///
    /// Set to `None` by default: Zebra only checks state format validity on startup and shutdown.
    #[serde(with = "humantime_serde")]
    pub debug_validity_check_interval: Option<Duration>,

    // Elasticsearch configs
    //
    #[cfg(feature = "elasticsearch")]
    /// The elasticsearch database url.
    pub elasticsearch_url: String,

    #[cfg(feature = "elasticsearch")]
    /// The elasticsearch database username.
    pub elasticsearch_username: String,

    #[cfg(feature = "elasticsearch")]
    /// The elasticsearch database password.
    pub elasticsearch_password: String,
}

fn gen_temp_path(prefix: &str) -> PathBuf {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("temporary directory is created successfully")
        .keep()
}

impl Config {
    /// Returns the path for the database, based on the kind, major version and network.
    /// Each incompatible database format or network gets its own unique path.
    pub fn db_path(
        &self,
        db_kind: impl AsRef<str>,
        major_version: u64,
        network: &Network,
    ) -> PathBuf {
        let db_kind = db_kind.as_ref();
        let major_version = format!("v{}", major_version);
        let net_dir = network.lowercase_name();

        if self.ephemeral {
            gen_temp_path(&format!("zebra-{db_kind}-{major_version}-{net_dir}-"))
        } else {
            self.cache_dir
                .join(db_kind)
                .join(major_version)
                .join(net_dir)
        }
    }

    /// Returns the path for the database format minor/patch version file,
    /// based on the kind, major version and network.
    pub fn version_file_path(
        &self,
        db_kind: impl AsRef<str>,
        major_version: u64,
        network: &Network,
    ) -> PathBuf {
        let mut version_path = self.db_path(db_kind, major_version, network);

        version_path.push(DATABASE_FORMAT_VERSION_FILE_NAME);

        version_path
    }

    /// Returns a config for a temporary database that is deleted when it is dropped.
    pub fn ephemeral() -> Config {
        Config {
            ephemeral: true,
            ..Config::default()
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cache_dir: default_cache_dir(),
            ephemeral: false,
            delete_old_database: true,
            debug_stop_at_height: None,
            debug_validity_check_interval: None,
            #[cfg(feature = "elasticsearch")]
            elasticsearch_url: "https://localhost:9200".to_string(),
            #[cfg(feature = "elasticsearch")]
            elasticsearch_username: "elastic".to_string(),
            #[cfg(feature = "elasticsearch")]
            elasticsearch_password: "".to_string(),
        }
    }
}

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

    let mut db_path = config.db_path(&db_kind, major_version, network);
    // Check and remove the network path.
    assert_eq!(
        db_path.file_name(),
        Some(network.lowercase_name().as_ref()),
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
            let deleted_db = check_and_delete_database(&config, major_version, &entry);

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
    entry: &DirEntry,
) -> Option<PathBuf> {
    let dir_name = parse_dir_name(entry)?;
    let dir_major_version = parse_major_version(&dir_name)?;

    if dir_major_version >= major_version {
        return None;
    }

    // Don't delete databases that can be reused.
    if restorable_db_versions()
        .iter()
        .map(|v| v - 1)
        .any(|v| v == dir_major_version)
    {
        return None;
    }

    let outdated_path = entry.path();

    // # Correctness
    //
    // Check that the path we're about to delete is inside the cache directory.
    // If the user has symlinked the outdated state directory to a non-cache directory,
    // we don't want to delete it, because it might contain other files.
    //
    // We don't attempt to guard against malicious symlinks created by attackers
    // (TOCTOU attacks). Zebra should not be run with elevated privileges.
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

/// Returns the full semantic version of the on-disk database, based on its config, kind, major version,
/// and network.
///
/// Typically, the version is read from a version text file.
///
/// If there is an existing on-disk database, but no version file,
/// returns `Ok(Some(major_version.0.0))`.
/// (This happens even if the database directory was just newly created.)
///
/// If there is no existing on-disk database, returns `Ok(None)`.
///
/// This is the format of the data on disk, the version
/// implemented by the running Zebra code can be different.
pub fn database_format_version_on_disk(
    config: &Config,
    db_kind: impl AsRef<str>,
    major_version: u64,
    network: &Network,
) -> Result<Option<Version>, BoxError> {
    let version_path = config.version_file_path(&db_kind, major_version, network);
    let db_path = config.db_path(db_kind, major_version, network);

    database_format_version_at_path(&version_path, &db_path, major_version)
}

/// Returns the full semantic version of the on-disk database at `version_path`.
///
/// See [`database_format_version_on_disk()`] for details.
pub(crate) fn database_format_version_at_path(
    version_path: &Path,
    db_path: &Path,
    major_version: u64,
) -> Result<Option<Version>, BoxError> {
    let disk_version_file = match fs::read_to_string(version_path) {
        Ok(version) => Some(version),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            // If the version file doesn't exist, don't guess the version yet.
            None
        }
        Err(e) => Err(e)?,
    };

    // The database has a version file on disk
    if let Some(version) = disk_version_file {
        return Ok(Some(
            version
                .parse()
                // Try to parse the previous format of the disk version file if it cannot be parsed as a `Version` directly.
                .or_else(|err| {
                    format!("{major_version}.{version}")
                        .parse()
                        .map_err(|err2| format!("failed to parse format version: {err}, {err2}"))
                })?,
        ));
    }

    // There's no version file on disk, so we need to guess the version
    // based on the database content
    match fs::metadata(db_path) {
        // But there is a database on disk, so it has the current major version with no upgrades.
        // If the database directory was just newly created, we also return this version.
        Ok(_metadata) => Ok(Some(Version::new(major_version, 0, 0))),

        // There's no version file and no database on disk, so it's a new database.
        // It will be created with the current version,
        // but temporarily return the default version above until the version file is written.
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),

        Err(e) => Err(e)?,
    }
}

// Hide this destructive method from the public API, except in tests.
#[allow(unused_imports)]
pub(crate) use hidden::{
    write_database_format_version_to_disk, write_state_database_format_version_to_disk,
};

pub(crate) mod hidden {
    #![allow(dead_code)]

    use zebra_chain::common::atomic_write;

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

    /// Writes `changed_version` to the on-disk database after the format is changed.
    /// (Or a new database is created.)
    ///
    /// The database path is based on its kind, `major_version_in_code`, and network.
    ///
    /// # Correctness
    ///
    /// This should only be called:
    /// - after each format upgrade is complete,
    /// - when creating a new database, or
    /// - when an older Zebra version opens a newer database.
    ///
    /// # Concurrency
    ///
    /// This must only be called while RocksDB has an open database for `config`.
    /// Otherwise, multiple Zebra processes could write the version at the same time,
    /// corrupting the file.
    pub fn write_database_format_version_to_disk(
        config: &Config,
        db_kind: impl AsRef<str>,
        major_version_in_code: u64,
        changed_version: &Version,
        network: &Network,
    ) -> Result<(), BoxError> {
        // Write the version file atomically so the cache is not corrupted if Zebra shuts down or
        // crashes.
        atomic_write(
            config.version_file_path(db_kind, major_version_in_code, network),
            changed_version.to_string().as_bytes(),
        )??;

        Ok(())
    }
}
