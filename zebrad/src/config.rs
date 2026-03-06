//! Zebrad Config
//!
//! See instructions in `commands.rs` to specify the path to your
//! application's configuration file and/or command-line options
//! for specifying it.

use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Centralized, case-insensitive suffix-based deny-list to ban setting config fields with
/// environment variables if those config field names end with any of these suffixes.
const DENY_CONFIG_KEY_SUFFIX_LIST: [&str; 5] = [
    "password",
    "secret",
    "token",
    // Block raw cookies only if a field is literally named "cookie".
    // (Paths like cookie_dir are not affected.)
    "cookie",
    // Only raw private keys; paths like *_private_key_path are not affected.
    "private_key",
];

/// Returns true if a leaf key name should be considered sensitive and blocked
/// from environment variable overrides.
fn is_sensitive_leaf_key(leaf_key: &str) -> bool {
    let key = leaf_key.to_ascii_lowercase();
    DENY_CONFIG_KEY_SUFFIX_LIST
        .iter()
        .any(|deny_suffix| key.ends_with(deny_suffix))
}

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

    /// Health check HTTP server configuration.
    ///
    /// See the Zebra Book for details and examples:
    /// <https://zebra.zfnd.org/user/health.html>
    pub health: crate::components::health::Config,
}

impl ZebradConfig {
    /// Loads the configuration from the conventional sources.
    ///
    /// Configuration is loaded from three sources, in order of precedence:
    /// 1. Environment variables with `ZEBRA_` prefix (highest precedence)
    /// 2. TOML configuration file (if provided)
    /// 3. Hard-coded defaults (lowest precedence)
    ///
    /// Environment variables use the format `ZEBRA_SECTION__KEY` where:
    /// - `SECTION` is the configuration section (e.g., `network`, `rpc`)
    /// - `KEY` is the configuration key within that section
    /// - Double underscores (`__`) separate nested keys
    ///
    /// # Security
    ///
    /// Environment variables whose leaf key names end with sensitive suffixes (case-insensitive)
    /// will cause configuration loading to fail with an error: `password`, `secret`, `token`, `cookie`, `private_key`.
    /// This prevents both silent misconfigurations and process table exposure of sensitive values.
    ///
    /// See [`DENY_CONFIG_KEY_SUFFIX_LIST`] and [`is_sensitive_leaf_key()`] above
    ///
    /// # Examples
    /// - `ZEBRA_NETWORK__NETWORK=Testnet` sets `network.network = "Testnet"`
    /// - `ZEBRA_RPC__LISTEN_ADDR=127.0.0.1:8232` sets `rpc.listen_addr = "127.0.0.1:8232"`
    pub fn load(config_path: Option<PathBuf>) -> Result<Self, config::ConfigError> {
        Self::load_with_env(config_path, "ZEBRA")
    }

    /// Loads configuration using a caller-provided environment variable prefix.
    ///
    /// This allows callers that need multiple configs in the same process (e.g.,
    /// the `copy-state` command) to keep overrides separate. For example:
    /// - Source/base config uses `ZEBRA_...` env vars (default prefix)
    /// - Target config uses `ZEBRA_TARGET_...` env vars
    ///
    /// The nested key separator remains `__`, e.g., `ZEBRA_TARGET_STATE__CACHE_DIR`.
    pub fn load_with_env(
        config_path: Option<PathBuf>,
        env_prefix: &str,
    ) -> Result<Self, config::ConfigError> {
        // 1. Start with an empty `config::Config` builder (no pre-populated values).
        // We merge sources, then deserialize into `ZebradConfig`, which uses
        // `ZebradConfig::default()` wherever keys are missing.
        let mut builder = config::Config::builder();

        // 2. Add TOML configuration file as a source if provided
        if let Some(path) = config_path {
            builder = builder.add_source(
                config::File::from(path)
                    .format(config::FileFormat::Toml)
                    .required(true),
            );
        }

        // 3. Load from environment variables (with a sensitive-leaf deny-list)
        // Use the provided prefix and `__` as separator for nested keys.
        // We filter the raw environment first, then let config-rs parse types via try_parsing(true).
        let mut filtered_env: HashMap<String, String> = HashMap::new();
        let required_prefix = format!("{}_", env_prefix);
        for (key, value) in std::env::vars() {
            if let Some(without_prefix) = key.strip_prefix(&required_prefix) {
                // Check for sensitive keys on the stripped key.
                let parts: Vec<&str> = without_prefix.split("__").collect();
                if let Some(leaf) = parts.last() {
                    if is_sensitive_leaf_key(leaf) {
                        return Err(config::ConfigError::Message(format!(
                            "Environment variable '{}' contains sensitive key '{}' which cannot be overridden via environment variables. \
                             Use the configuration file instead to prevent process table exposure.",
                            key, leaf
                        )));
                    }
                }

                // When providing a `source` map, the keys should not have the prefix.
                filtered_env.insert(without_prefix.to_string(), value);
            }
        }

        // When using `source`, we provide a map of already-filtered and processed
        // keys, so we use a default `Environment` without a prefix.
        builder = builder.add_source(
            config::Environment::default()
                .separator("__")
                .try_parsing(true)
                .source(Some(filtered_env)),
        );

        // Build the configuration
        let config = builder.build()?;
        // Deserialize into our struct, which will use defaults for any missing fields
        config.try_deserialize()
    }
}
