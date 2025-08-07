//! Zebrad Config
//!
//! See instructions in `commands.rs` to specify the path to your
//! application's configuration file and/or command-line options
//! for specifying it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Configuration for `zebrad`.
///
/// The `zebrad` config is a TOML-encoded version of this structure. The meaning
/// of each field is described in the documentation, although it may be necessary
/// to click through to the sub-structures for each section.
///
/// The path to the configuration file can also be specified with the `--config` flag when running Zebra.
///
/// The default path to the `zebrad` config is platform dependent, based on
/// [`dirs::preference_dir`](https://docs.rs/dirs/latest/dirs/fn.preference_dir.html):
///
/// | Platform | Value                                 | Example                                        |
/// | -------- | ------------------------------------- | ---------------------------------------------- |
/// | Linux    | `$XDG_CONFIG_HOME` or `$HOME/.config` | `/home/alice/.config/zebrad.toml`              |
/// | macOS    | `$HOME/Library/Preferences`           | `/Users/Alice/Library/Preferences/zebrad.toml` |
/// | Windows  | `{FOLDERID_RoamingAppData}`           | `C:\Users\Alice\AppData\Local\zebrad.toml`     |
#[derive(Clone, Default, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct ZebradConfig {
    /// Consensus configuration
    //
    // These configs use full paths to avoid a rustdoc link bug (#7048).
    pub consensus: zebra_consensus::config::Config,

    /// Metrics configuration
    pub metrics: crate::components::metrics::Config,

    /// Networking configuration
    pub network: zebra_network::config::Config,

    /// State configuration
    pub state: zebra_state::config::Config,

    /// Tracing configuration
    pub tracing: crate::components::tracing::Config,

    /// Sync configuration
    pub sync: crate::components::sync::Config,

    /// Mempool configuration
    pub mempool: crate::components::mempool::Config,

    /// RPC configuration
    pub rpc: zebra_rpc::config::rpc::Config,

    /// Mining configuration
    pub mining: zebra_rpc::config::mining::Config,
}

impl ZebradConfig {
    /// Loads the configuration from the conventional sources.
    ///
    /// Configuration is loaded from three sources, in order of precedence:
    /// 1. Hard-coded defaults (lowest precedence)
    /// 2. TOML configuration file (if provided)
    /// 3. Environment variables with `ZEBRA_` prefix (highest precedence)
    ///
    /// Environment variables use the format `ZEBRA_SECTION__KEY` where:
    /// - `SECTION` is the configuration section (e.g., `network`, `rpc`)
    /// - `KEY` is the configuration key within that section
    /// - Double underscores (`__`) separate nested keys
    ///
    /// # Examples
    /// - `ZEBRA_NETWORK__NETWORK=Testnet` sets `network.network = "Testnet"`
    /// - `ZEBRA_RPC__LISTEN_ADDR=127.0.0.1:8232` sets `rpc.listen_addr = "127.0.0.1:8232"`
    pub fn load(config_path: Option<PathBuf>) -> Result<Self, config::ConfigError> {
        let mut builder = config::Config::builder();

        // 1. Start with defaults - but don't use try_from with the struct directly
        // Instead, we'll let config-rs use its own defaults and override as needed

        // 2. Load from TOML file if provided
        if let Some(path) = config_path {
            builder = builder.add_source(config::File::from(path).required(true));
        }

        // 3. Load from standard ZEBRA_ environment variables, which will override legacy vars
        // Use ZEBRA_ prefix and __ as separator for nested keys
        builder = builder.add_source(
            config::Environment::with_prefix("ZEBRA")
                .prefix_separator("_")
                .separator("__")
                .try_parsing(true),
        );

        // Build the configuration
        let config = builder.build()?;

        // Deserialize into our struct, which will use the Default implementations
        // for any missing fields
        config.try_deserialize()
    }
}
