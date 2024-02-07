//! Configuration for blockchain scanning tasks.

use std::{fmt::Debug, net::SocketAddr};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use zebra_state::Config as DbConfig;

use crate::storage::SaplingScanningKey;

#[derive(Clone, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
/// Configuration for scanning.
pub struct Config {
    /// The sapling keys to scan for and the birthday height of each of them.
    ///
    /// Currently only supports Extended Full Viewing Keys in ZIP-32 format.
    //
    // TODO: allow keys without birthdays
    pub sapling_keys_to_scan: IndexMap<SaplingScanningKey, u32>,

    /// IP address and port for the zebra-scan gRPC server.
    ///
    /// Note: The gRPC server is disabled by default.
    /// To enable the gRPC server, set a listen address in the config:
    /// ```toml
    /// [shielded-scan]
    /// listen_addr = '127.0.0.1:8231'
    /// ```
    ///
    /// The recommended ports for the gRPC server are:
    /// - Mainnet: 127.0.0.1:8231
    /// - Testnet: 127.0.0.1:18231
    pub listen_addr: Option<SocketAddr>,

    /// The scanner results database config.
    //
    // TODO: Remove fields that are only used by the state, and create a common database config.
    #[serde(flatten)]
    db_config: DbConfig,
}

impl Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            // Security: don't log private keys, birthday heights might also be private
            .field("sapling_keys_to_scan", &self.sapling_keys_to_scan.len())
            .field("db_config", &self.db_config)
            .finish()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sapling_keys_to_scan: IndexMap::new(),
            listen_addr: None,

            // TODO: Add a const generic for specifying the default cache_dir path, like 'zebra' or 'zebra-scan'?
            db_config: DbConfig::default(),
        }
    }
}

impl Config {
    /// Returns a config for a temporary database that is deleted when it is dropped.
    pub fn ephemeral() -> Self {
        Self {
            db_config: DbConfig::ephemeral(),
            ..Self::default()
        }
    }

    /// Returns the database-specific config.
    pub fn db_config(&self) -> &DbConfig {
        &self.db_config
    }

    /// Returns the database-specific config as mutable.
    pub fn db_config_mut(&mut self) -> &mut DbConfig {
        &mut self.db_config
    }
}
