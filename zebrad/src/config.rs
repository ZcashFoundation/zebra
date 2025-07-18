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
    /// Load a configuration from an optional path and environment variables.
    ///
    /// If no path is provided, uses the default configuration location.
    ///
    /// ## Configuration Priority
    ///
    /// In normal mode:
    /// 1. Defaults (lowest priority)
    /// 2. TOML config file
    /// 3. Environment variables (highest priority)
    ///
    /// In test mode (when `TEST_MODE=1`):
    /// 1. Defaults (lowest priority)
    /// 2. Environment variables
    /// 3. TOML config file (highest priority)
    ///
    /// Test mode is enabled for:
    /// - Unit/integration tests via `zebra_test::init()`
    /// - Cargo test runs in Docker via entrypoint script
    ///
    /// This ensures that test configurations are not accidentally overridden
    /// by environment variables, while allowing CI tests that run zebrd directly
    /// to use normal environment variable override behavior.
    pub fn load(config_file_path: Option<PathBuf>) -> Result<Self, Error> {
        let default_config_file = Self::default_config_path();

        let config_file_path = config_file_path.unwrap_or(default_config_file);

        // Check if we're in test mode
        let is_test_mode = std::env::var("TEST_MODE").unwrap_or_default() == "1";

        // Create base figment with layered configuration
        let mut figment = Figment::new().merge(Serialized::defaults(Self::default()));

        if is_test_mode {
            // In test mode: prioritize config file over environment variables
            // This ensures test configurations aren't overridden by Docker/CI environment

            // Add environment variables first (lower priority)
            figment = figment.merge(Env::prefixed("ZEBRA_").split("__"));

            // Then add TOML file if it exists (higher priority - will override env vars)
            if config_file_path.exists() {
                figment = figment.merge(Toml::file(&config_file_path));
            }
        } else {
            // Normal mode: environment variables have higher priority

            // Only add TOML file if it exists - without .nested() to allow proper overriding
            if config_file_path.exists() {
                figment = figment.merge(Toml::file(&config_file_path));
            }

            // Add standard environment variables (without custom mappings first)
            figment = figment.merge(Env::prefixed("ZEBRA_").split("__"));
        }

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
