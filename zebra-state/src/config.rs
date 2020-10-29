use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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

    /// Controls the size of the database cache, in bytes.
    ///
    /// This corresponds to `sled`'s [`cache_capacity`][cc] parameter.
    /// Note that the behavior of this parameter is [somewhat
    /// unintuitive][gh], measuring the on-disk size of the cached data,
    /// not the in-memory size, which may be much larger, especially for
    /// smaller keys and values.
    ///
    /// [cc]: https://docs.rs/sled/0.34.4/sled/struct.Config.html#method.cache_capacity
    /// [gh]: https://github.com/spacejam/sled/issues/986#issuecomment-592950100
    pub memory_cache_bytes: u64,

    /// Whether to use an ephemeral database.
    ///
    /// Ephemeral databases are stored in memory on Linux, and in a temporary directory on other OSes.
    ///
    /// Set to `false` by default. If this is set to `true`, [`cache_dir`] is ignored.
    ///
    /// [`cache_dir`]: struct.Config.html#structfield.cache_dir
    pub ephemeral: bool,

    /// Commit blocks to the finalized state up to this height, then exit Zebra.
    ///
    /// If `None`, continue syncing indefinitely.
    pub debug_stop_at_height: Option<u32>,
}

impl Config {
    /// Generate the appropriate `sled::Config` for `network`, based on the
    /// provided `zebra_state::Config`.
    pub(crate) fn sled_config(&self, network: Network) -> sled::Config {
        let net_dir = match network {
            Network::Mainnet => "mainnet",
            Network::Testnet => "testnet",
        };

        let config = sled::Config::default()
            .cache_capacity(self.memory_cache_bytes)
            .mode(sled::Mode::LowSpace);

        if self.ephemeral {
            config.temporary(self.ephemeral)
        } else {
            let path = self
                .cache_dir
                .join("state")
                .join(format!("v{}", crate::constants::SLED_FORMAT_VERSION))
                .join(net_dir);

            config.path(path)
        }
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
            memory_cache_bytes: 50_000_000,
            ephemeral: false,
            debug_stop_at_height: None,
        }
    }
}
