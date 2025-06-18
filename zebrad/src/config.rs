//! Zebrad Config
//!
//! Configuration for the `zebrad` node, including how to load and override settings.
//!
//! # Configuration Loading Order
//!
//! Configuration is loaded in the following order of precedence:
//!
//! 1. **Defaults**: All fields have sensible defaults.
//! 2. **TOML File**: If a config file is present (default path is platform-dependent, see below), it overrides the defaults.
//! 3. **Environment Variables**: Any field can be overridden by an environment variable. Environment variables take highest precedence.
//!
//! # Environment Variable Overrides
//!
//! - All environment variables must be prefixed with `ZEBRA_`.
//! - Nested fields are specified using double underscores (`__`). For example:
//!   - `ZEBRA_RPC__LISTEN_ADDR` overrides `rpc.listen_addr`.
//!   - `ZEBRA_NETWORK__NETWORK` overrides `network.network`.
//!   - `ZEBRA_MINING__MINER_ADDRESS` overrides `mining.miner_address`.
//! - All config fields can be overridden this way, matching the TOML structure.
//! - See the integration tests in `zebra/zebrad/tests/config.rs` for concrete examples.
//!
//! # Config File Path
//!
//! The path to the configuration file can be specified with the `--config` flag when running Zebra.
//!
//! The default path to the `zebrad` config is platform dependent, based on
//! [`dirs::preference_dir`](https://docs.rs/dirs/latest/dirs/fn.preference_dir.html):
//!
//! | Platform | Value                                 | Example                                        |
//! | -------- | ------------------------------------- | ---------------------------------------------- |
//! | Linux    | `$XDG_CONFIG_HOME` or `$HOME/.config` | `/home/alice/.config/zebrad.toml`              |
//! | macOS    | `$HOME/Library/Preferences`           | `/Users/Alice/Library/Preferences/zebrad.toml` |
//! | Windows  | `{FOLDERID_RoamingAppData}`           | `C:\Users\Alice\AppData\Local\zebrad.toml`     |

use std::path::PathBuf;

use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};

/// Error type for configuration parsing.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("configuration error: {0}")]
pub struct Error(String);

impl From<figment::Error> for Error {
    fn from(err: figment::Error) -> Self {
        Self(err.to_string())
    }
}

/// Configuration for `zebrad`.
///
/// The `zebrad` config is a TOML-encoded version of this structure. The meaning
/// of each field is described in the documentation, although it may be necessary
/// to click through to the sub-structures for each section.
///
/// Configuration is loaded using the precedence order: Defaults → TOML → Environment Variables.
/// All fields can be overridden using environment variables with the `ZEBRA_` prefix and
/// double underscores (`__`) for nested fields (e.g., `ZEBRA_RPC__LISTEN_ADDR`).
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
    /// Loads configuration using Figment: Defaults → TOML → Environment Variables.
    ///
    /// The TOML file path can be specified via config_file_path parameter.
    /// If None, uses default path based on platform.
    ///
    /// Environment variables are prefixed with "ZEBRA_" and use double underscores
    /// for nested fields (e.g., ZEBRA_RPC__LISTEN_ADDR).
    pub fn load(config_file_path: Option<PathBuf>) -> Result<Self, Error> {
        let default_config_file = Self::default_config_path();

        let config_file_path = config_file_path.unwrap_or(default_config_file);

        // Create base figment with layered configuration
        let mut figment = Figment::new().merge(Serialized::defaults(Self::default()));

        // Only add TOML file if it exists - without .nested() to allow proper overriding
        if config_file_path.exists() {
            figment = figment.merge(Toml::file(&config_file_path));
        }

        // Add standard environment variables (without custom mappings first)
        figment = figment.merge(Env::prefixed("ZEBRA_").split("__"));

        let config: Self = figment.extract().map_err(Error::from)?;

        Ok(config)
    }

    /// Returns the default configuration file path based on platform.
    fn default_config_path() -> PathBuf {
        if let Some(config_dir) = dirs::preference_dir() {
            config_dir.join("zebrad.toml")
        } else {
            PathBuf::from("zebrad.toml")
        }
    }
}
