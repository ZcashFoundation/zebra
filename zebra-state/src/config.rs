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

use zebra_chain::parameters::Network;

use crate::{
    constants::{
        DATABASE_FORMAT_MINOR_VERSION, DATABASE_FORMAT_PATCH_VERSION, DATABASE_FORMAT_VERSION,
        DATABASE_FORMAT_VERSION_FILE_NAME,
    },
    BoxError,
};

/// Configuration for the state service.
#[derive(Clone, Debug, Deserialize, Serialize)]
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
        .into_path()
}

impl Config {
    /// Returns the path for the finalized state database
    pub fn db_path(&self, network: Network) -> PathBuf {
        let net_dir = network.lowercase_name();

        if self.ephemeral {
            gen_temp_path(&format!(
                "zebra-state-v{}-{}-",
                crate::constants::DATABASE_FORMAT_VERSION,
                net_dir
            ))
        } else {
            self.cache_dir
                .join("state")
                .join(format!("v{}", crate::constants::DATABASE_FORMAT_VERSION))
                .join(net_dir)
        }
    }

    /// Returns the path of the database format version file.
    pub fn version_file_path(&self, network: Network) -> PathBuf {
        let mut version_path = self.db_path(network);

        version_path.push(DATABASE_FORMAT_VERSION_FILE_NAME);

        version_path
    }

    /// Construct a config for an ephemeral database
    pub fn ephemeral() -> Config {
        Config {
            ephemeral: true,
            ..Config::default()
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| std::env::current_dir().unwrap().join("cache"))
            .join("zebra");

        Self {
            cache_dir,
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

/// Spawns a task that checks if there are old database folders,
/// and deletes them from the filesystem.
///
/// Iterate over the files and directories in the databases folder and delete if:
/// - The state directory exists.
/// - The entry is a directory.
/// - The directory name has a prefix `v`.
/// - The directory name without the prefix can be parsed as an unsigned number.
/// - The parsed number is lower than the hardcoded `DATABASE_FORMAT_VERSION`.
pub fn check_and_delete_old_databases(config: Config) -> JoinHandle<()> {
    let current_span = Span::current();

    spawn_blocking(move || {
        current_span.in_scope(|| {
            delete_old_databases(config);
            info!("finished old database version cleanup task");
        })
    })
}

/// Check if there are old database folders and delete them from the filesystem.
///
/// See [`check_and_delete_old_databases`] for details.
fn delete_old_databases(config: Config) {
    if config.ephemeral || !config.delete_old_database {
        return;
    }

    info!("checking for old database versions");

    let state_dir = config.cache_dir.join("state");
    if let Some(state_dir) = read_dir(&state_dir) {
        for entry in state_dir.flatten() {
            let deleted_state = check_and_delete_database(&config, &entry);

            if let Some(deleted_state) = deleted_state {
                info!(?deleted_state, "deleted outdated state directory");
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
fn check_and_delete_database(config: &Config, entry: &DirEntry) -> Option<PathBuf> {
    let dir_name = parse_dir_name(entry)?;
    let version_number = parse_version_number(&dir_name)?;

    if version_number >= crate::constants::DATABASE_FORMAT_VERSION {
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

/// Parse the state version number from `dir_name`.
///
/// Returns `None` if parsing fails, or the directory name is not in the expected format.
fn parse_version_number(dir_name: &str) -> Option<u64> {
    dir_name
        .strip_prefix('v')
        .and_then(|version| version.parse().ok())
}

// TODO: move these to the format upgrade module

/// Returns the full semantic version of the currently running database format code.
///
/// This is the version implemented by the Zebra code that's currently running,
/// the minor and patch versions on disk can be different.
pub fn database_format_version_in_code() -> Version {
    Version::new(
        DATABASE_FORMAT_VERSION,
        DATABASE_FORMAT_MINOR_VERSION,
        DATABASE_FORMAT_PATCH_VERSION,
    )
}

/// Returns the full semantic version of the on-disk database.
///
/// Typically, the version is read from a version text file.
///
/// If there is an existing on-disk database, but no version file, returns `Ok(Some(major.0.0))`.
/// (This happens even if the database directory was just newly created.)
///
/// If there is no existing on-disk database, returns `Ok(None)`.
///
/// This is the format of the data on disk, the minor and patch versions
/// implemented by the running Zebra code can be different.
pub fn database_format_version_on_disk(
    config: &Config,
    network: Network,
) -> Result<Option<Version>, BoxError> {
    let version_path = config.version_file_path(network);
    let db_path = config.db_path(network);

    database_format_version_at_path(&version_path, &db_path)
}

/// Returns the full semantic version of the on-disk database at `version_path`.
///
/// See [`database_format_version_on_disk()`] for details.
pub(crate) fn database_format_version_at_path(
    version_path: &Path,
    db_path: &Path,
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
        let (minor, patch) = version
            .split_once('.')
            .ok_or("invalid database format version file")?;

        return Ok(Some(Version::new(
            DATABASE_FORMAT_VERSION,
            minor.parse()?,
            patch.parse()?,
        )));
    }

    // There's no version file on disk, so we need to guess the version
    // based on the database content
    match fs::metadata(db_path) {
        // But there is a database on disk, so it has the current major version with no upgrades.
        // If the database directory was just newly created, we also return this version.
        Ok(_metadata) => Ok(Some(Version::new(DATABASE_FORMAT_VERSION, 0, 0))),

        // There's no version file and no database on disk, so it's a new database.
        // It will be created with the current version,
        // but temporarily return the default version above until the version file is written.
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),

        Err(e) => Err(e)?,
    }
}

/// Writes `changed_version` to the on-disk database after the format is changed.
/// (Or a new database is created.)
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
///
/// # Panics
///
/// If the major versions do not match. (The format is incompatible.)
pub fn write_database_format_version_to_disk(
    changed_version: &Version,
    config: &Config,
    network: Network,
) -> Result<(), BoxError> {
    let version_path = config.version_file_path(network);

    // The major version is already in the directory path.
    assert_eq!(
        changed_version.major, DATABASE_FORMAT_VERSION,
        "tried to do in-place database format change to an incompatible version"
    );

    let version = format!("{}.{}", changed_version.minor, changed_version.patch);

    // # Concurrency
    //
    // The caller handles locking for this file write.
    fs::write(version_path, version.as_bytes())?;

    Ok(())
}
