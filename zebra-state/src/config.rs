//! Cached state configuration for Zebra.

use std::{
    fmt,
    fs::{self, canonicalize, remove_dir_all, DirEntry, ReadDir},
    io::ErrorKind,
    path::{Path, PathBuf},
    time::Duration,
};

use semver::Version;
use serde::{
    de::{self, IgnoredAny, MapAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use tokio::task::{spawn_blocking, JoinHandle};
use tracing::Span;

use zebra_chain::{common::default_cache_dir, parameters::Network};

use crate::{
    constants::{
        min_pruning_retention, DATABASE_FORMAT_VERSION_FILE_NAME, MIN_PRUNING_RETENTION,
        STATE_DATABASE_KIND,
    },
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
    /// Storage mode is controlled separately by [`storage_mode`](Config::storage_mode).
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

    /// Whether to cache non-finalized blocks on disk to be restored when Zebra restarts.
    ///
    /// Set to `true` by default. If this is set to `false`, Zebra will irrecoverably drop
    /// non-finalized blocks when the process exits and will have to re-download them from
    /// the network when it restarts, if those blocks are still available in the network.
    ///
    /// Note: The non-finalized state will be written to a backup cache once per 5 seconds at most.
    ///       If blocks are added to the non-finalized state more frequently, the backup may not reflect
    ///       Zebra's last non-finalized state before it shut down.
    pub should_backup_non_finalized_state: bool,

    /// Whether committed full blocks should seed the Zakura header-only store.
    ///
    /// This is enabled by zebrad only for the Zakura v2 path. It is skipped in
    /// serde so the generic Zebra state config does not expose a Zakura-specific
    /// user setting.
    #[serde(skip)]
    pub enable_zakura_header_seed_from_committed_blocks: bool,

    /// Whether to delete the old database directories when present.
    ///
    /// Set to `true` by default. If this is set to `false`,
    /// no check for old database versions will be made and nothing will be
    /// deleted.
    pub delete_old_database: bool,

    /// Selects whether Zebra keeps all historical block data, or stores only the
    /// data required to validate future blocks.
    ///
    /// Set to [`StorageMode::Archive`] by default, which preserves all data and
    /// is required to answer historical RPC queries.
    ///
    /// [`StorageMode::Pruned`] deletes historical raw transaction bytes outside a
    /// retention window to reduce disk usage, while keeping all consensus-critical
    /// state (the UTXO set, nullifiers, anchors, note commitment trees, history
    /// tree, and value pools) as well as the transaction location indexes needed
    /// to validate future spends. This is a one-way mode: once data has been
    /// pruned, the database cannot be reopened in [`StorageMode::Archive`] without
    /// re-syncing from genesis.
    pub storage_mode: StorageMode,

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

    /// If true, skip spawning the non-finalized state backup task and instead write
    /// the non-finalized state to the backup directory synchronously before each update
    /// to the latest chain tip or non-finalized state channels.
    ///
    /// Set to `false` by default. When `true`, the non-finalized state is still restored
    /// from the backup directory on startup, but updates are written synchronously on every
    /// block commit rather than asynchronously every 5 seconds.
    ///
    /// This is intended for testing scenarios where blocks are committed rapidly and the
    /// async backup task may not flush all blocks before shutdown.
    pub debug_skip_non_finalized_state_backup_task: bool,

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
        let major_version = format!("v{major_version}");
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

    /// Returns the path for the non-finalized state backup directory, based on the network.
    /// Non-finalized state backup files are encoded in the network protocol format and remain
    /// valid across db format upgrades.
    pub fn non_finalized_state_backup_dir(&self, network: &Network) -> Option<PathBuf> {
        if self.ephemeral || !self.should_backup_non_finalized_state {
            // Ephemeral databases are intended to be irrecoverable across restarts and don't
            // require a backup for the non-finalized state.
            return None;
        }

        let net_dir = network.lowercase_name();
        Some(self.cache_dir.join("non_finalized_state").join(net_dir))
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

    /// Returns the [`PruningConfig`] if this config selects pruned storage mode.
    pub fn pruning_config(&self) -> Option<&PruningConfig> {
        match &self.storage_mode {
            StorageMode::Archive => None,
            StorageMode::Pruned(pruning) => Some(pruning),
        }
    }

    /// Validates the configured [`StorageMode`].
    ///
    /// This must be called before opening the database, so that a misconfigured
    /// retention window fails fast at startup rather than mid-run.
    ///
    /// # Errors
    ///
    /// Returns an error if pruned mode is selected with a `tx_retention` below
    /// the network-specific floor ([`min_pruning_retention`]). A retention window
    /// at or below the reorg depth could let pruning delete data that a rollback
    /// needs to read.
    pub fn validate_storage_mode(&self, network: &Network) -> Result<(), BoxError> {
        if let Some(pruning) = self.pruning_config() {
            let floor = min_pruning_retention(network);
            if pruning.tx_retention < floor {
                return Err(format!(
                    "invalid pruning configuration: tx_retention ({}) must be at least {floor} \
                     on {network} so pruning cannot delete data within the reorg/rollback window",
                    pruning.tx_retention,
                )
                .into());
            }
        }

        Ok(())
    }
}

/// Selects whether Zebra keeps all historical block data, or stores only the data
/// required to validate future blocks.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    /// Keep all historical data, including raw transactions and lookup indexes.
    ///
    /// This is the default and is required to answer historical RPC queries such
    /// as `getrawtransaction` for old transactions.
    #[default]
    Archive,

    /// Prune historical raw transaction bytes outside the retention window.
    ///
    /// Consensus-critical state and transaction location indexes are always
    /// retained. This is a one-way mode: a pruned database cannot be reopened in
    /// [`StorageMode::Archive`].
    Pruned(PruningConfig),
}

impl<'de> Deserialize<'de> for StorageMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            deserializer.deserialize_any(StorageModeVisitor)
        } else {
            StorageModeSerde::deserialize(deserializer).map(Into::into)
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum StorageModeSerde {
    Archive,
    Pruned(PruningConfig),
}

impl From<StorageModeSerde> for StorageMode {
    fn from(storage_mode: StorageModeSerde) -> Self {
        match storage_mode {
            StorageModeSerde::Archive => StorageMode::Archive,
            StorageModeSerde::Pruned(pruning) => StorageMode::Pruned(pruning),
        }
    }
}

struct StorageModeVisitor;

impl<'de> Visitor<'de> for StorageModeVisitor {
    type Value = StorageMode;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(r#"`"archive"`, `"pruned"`, or a storage mode table"#)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match value {
            "archive" => Ok(StorageMode::Archive),
            "pruned" => Ok(StorageMode::Pruned(PruningConfig::default())),
            _ => Err(de::Error::unknown_variant(value, &["archive", "pruned"])),
        }
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let Some(key) = map.next_key::<String>()? else {
            return Err(de::Error::invalid_length(0, &self));
        };

        let storage_mode = match key.as_str() {
            "archive" => {
                map.next_value::<IgnoredAny>()?;
                StorageMode::Archive
            }
            "pruned" => StorageMode::Pruned(map.next_value()?),
            _ => return Err(de::Error::unknown_variant(&key, &["archive", "pruned"])),
        };

        if let Some(key) = map.next_key::<String>()? {
            return Err(de::Error::custom(format!(
                "multiple storage mode variants configured, including `{key}`"
            )));
        }

        Ok(storage_mode)
    }
}

/// Configuration for [`StorageMode::Pruned`].
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct PruningConfig {
    /// Number of recent finalized blocks below the tip whose raw transaction data
    /// is retained.
    ///
    /// Blocks older than this window have their raw transaction bytes (`tx_by_loc`)
    /// deleted. Transaction lookup indexes (`tx_loc_by_hash`, `hash_by_tx_loc`) are
    /// retained, because they are needed to resolve spends of UTXOs created in old
    /// blocks. Must be at least [`MIN_PRUNING_RETENTION`]; this is enforced by
    /// [`Config::validate_storage_mode`].
    pub tx_retention: u32,
}

impl Default for PruningConfig {
    fn default() -> Self {
        Self {
            tx_retention: MIN_PRUNING_RETENTION,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cache_dir: default_cache_dir(),
            ephemeral: false,
            should_backup_non_finalized_state: true,
            enable_zakura_header_seed_from_committed_blocks: false,
            delete_old_database: true,
            storage_mode: StorageMode::default(),
            debug_stop_at_height: None,
            debug_validity_check_interval: None,
            debug_skip_non_finalized_state_backup_task: false,
            #[cfg(feature = "elasticsearch")]
            elasticsearch_url: "https://localhost:9200".to_string(),
            #[cfg(feature = "elasticsearch")]
            elasticsearch_username: "elastic".to_string(),
            #[cfg(feature = "elasticsearch")]
            elasticsearch_password: "".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_mode_deserializes_from_documented_toml() {
        let archive: Config = toml::from_str(r#"storage_mode = "archive""#)
            .expect("archive storage mode deserializes from a string");
        assert_eq!(archive.storage_mode, StorageMode::Archive);

        let pruned: Config = toml::from_str(r#"storage_mode = "pruned""#)
            .expect("pruned storage mode deserializes from a string");
        assert_eq!(
            pruned.storage_mode,
            StorageMode::Pruned(PruningConfig::default())
        );

        let pruned_with_retention: Config = toml::from_str(
            r#"
            [storage_mode.pruned]
            tx_retention = 6000
            "#,
        )
        .expect("pruned storage mode deserializes from a table");
        assert_eq!(
            pruned_with_retention.storage_mode,
            StorageMode::Pruned(PruningConfig { tx_retention: 6000 })
        );
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

    let restorable_db_versions = restorable_db_versions();

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
