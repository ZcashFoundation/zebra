use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
        }
    }
}
