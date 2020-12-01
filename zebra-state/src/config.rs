use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tempdir::TempDir;
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
    /// Each network has a separate state, which is stored in "mainnet/state"
    /// and "testnet/state" subdirectories.
    ///
    /// The default directory is platform dependent, based on
    /// [`dirs::cache_dir()`](https://docs.rs/dirs/3.0.1/dirs/fn.cache_dir.html):
    ///
    /// |Platform | Value                                           | Example                            |
    /// | ------- | ----------------------------------------------- | ---------------------------------- |
    /// | Linux   | `$XDG_CACHE_HOME/zebra` or `$HOME/.cache/zebra` | /home/alice/.cache/zebra           |
    /// | macOS   | `$HOME/Library/Caches/zebra`                    | /Users/Alice/Library/Caches/zebra  |
    /// | Windows | `{FOLDERID_LocalAppData}\zebra`                 | C:\Users\Alice\AppData\Local\zebra |
    /// | Other   | `std::env::current_dir()/cache`                 |                                    |
    pub cache_dir: PathBuf,

    /// Whether to use an ephemeral database.
    ///
    /// Ephemeral databases are stored in a temporary directory.
    /// They are deleted when Zebra exits successfully.
    /// (If Zebra panics or crashes, the ephemeral database won't be deleted.)
    ///
    /// Set to `false` by default. If this is set to `true`, [`cache_dir`] is ignored.
    ///
    /// [`cache_dir`]: struct.Config.html#structfield.cache_dir
    pub ephemeral: bool,

    /// Commit blocks to the finalized state up to this height, then exit Zebra.
    ///
    /// Set to `None` by default: Zebra continues syncing indefinitely.
    pub debug_stop_at_height: Option<u32>,
}

fn gen_temp_path(prefix: &str) -> PathBuf {
    TempDir::new(prefix)
        .expect("temporary directory is created successfully")
        .into_path()
}

impl Config {
    /// Returns the path and database options for the finalized state database
    pub(crate) fn db_config(&self, network: Network) -> (PathBuf, rocksdb::Options) {
        let net_dir = match network {
            Network::Mainnet => "mainnet",
            Network::Testnet => "testnet",
        };

        let path = if self.ephemeral {
            gen_temp_path(&format!(
                "zebra-state-v{}-{}",
                crate::constants::DATABASE_FORMAT_VERSION,
                net_dir
            ))
        } else {
            self.cache_dir
                .join("state")
                .join(format!("v{}", crate::constants::DATABASE_FORMAT_VERSION))
                .join(net_dir)
        };

        let mut opts = rocksdb::Options::default();

        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        (path, opts)
    }

    /// Construct a config for an ephemeral in memory database
    pub fn ephemeral() -> Self {
        let mut config = Self::default();
        config.ephemeral = true;
        config
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
        }
    }
}
