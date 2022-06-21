//! Cached state configuration for Zebra.

use std::{
    fs::{canonicalize, remove_dir_all, DirEntry, ReadDir},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tokio::task::{spawn_blocking, JoinHandle};
use tracing::Span;

use zebra_chain::parameters::Network;

/// Configuration for the state service.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The root directory for storing cached data.
    ///
    /// Cached data includes any state that can be replicated from the network
    /// (e.g., the chain state, the blocks, the UTXO set, etc.). It does *not*
    /// include private data that cannot be replicated from the network, such as
    /// wallet data.  That data is not handled by `zebra-state`.
    ///
    /// Each state format version and network has a separate state.
    /// These states are stored in `state/vN/mainnet` and `state/vN/testnet` subdirectories,
    /// underneath the `cache_dir` path, where `N` is the state format version.
    ///
    /// When Zebra's state format changes, it creates a new state subdirectory for that version,
    /// and re-syncs from genesis.
    ///
    /// Old state versions are [not automatically deleted](https://github.com/ZcashFoundation/zebra/issues/1213).
    /// It is ok to manually delete old state versions.
    ///
    /// It is also ok to delete the entire cached state directory.
    /// If you do, Zebra will re-sync from genesis next time it is launched.
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

    /// Commit blocks to the finalized state up to this height, then exit Zebra.
    ///
    /// Set to `None` by default: Zebra continues syncing indefinitely.
    pub debug_stop_at_height: Option<u32>,

    /// Whether to delete the old database directories when present.
    ///
    /// Set to `true` by default. If this is set to `false`,
    /// no check for old database versions will be made and nothing will be
    /// deleted.
    pub delete_old_database: bool,
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
        let net_dir = match network {
            Network::Mainnet => "mainnet",
            Network::Testnet => "testnet",
        };

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
            debug_stop_at_height: None,
            delete_old_database: true,
        }
    }
}

// Cleaning up old database versions

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
fn parse_version_number(dir_name: &str) -> Option<u32> {
    dir_name
        .strip_prefix('v')
        .and_then(|version| version.parse().ok())
}
